//! One Machine's floor for Service `app/api`, and every use of an approval that reads it.
//!
//! The floor lives in the Machine's `ConsumptionLog` (`restart-approvals.json`), so a
//! crash keeps it and loses only work in flight. Each actor step is one floor read or one
//! write of the code it copies, and each read calls the `ployz_decide::service_floor`
//! decision the daemon calls:
//!
//! - A forget (`LocalMachine::forget_versions`, forget.rs, through
//!   `ConsumptionLog::raise_floors`): `raise` at the Machine's clock. One is signed at 2,
//!   and one eleven minutes past the clock, as a Device whose clock runs fast signs it.
//! - An admission of a new Container, in three steps: the first floor read
//!   (`ConsumptionLog::admit_versions` from `approved_versions` in admission.rs for a
//!   version, `rule_floor` in rule_deploy.rs for a rule), the image pull, then
//!   `AdmittedSpec::still_holds` right before Docker creates it (docker/lifecycle.rs). The
//!   approvals: version V1 signed at 1, the forget's own version V2 signed at 2, whose
//!   first read also raises the floor, rule R1 issued at 1 and reached through a verified
//!   build, rule R2 approved again at 3, and W, a version of the sibling Service `app/web`
//!   at 1.
//! - An Operator's start of a stopped Container (`LocalMachine::start_checked`), one
//!   `Standing::admits` read: K1, admitted by key A signing at 1 and key B signing at 3,
//!   and KR, admitted under R1.
//! - The removal of key B, the clock ticking, and a crash.
//!
//! What a check passed at its read stands until Docker acts on it. A forget that lands in
//! between is the accepted handoff race, so the properties hold each check to the floor it
//! read, never to a later one.
//!
//! Out of scope: a second Machine (each one decides alone), the forget fan-out, and the
//! signature checks before the floor (the authority tests cover them).

use std::fmt::Write as _;

use ployz_core::authority::Timestamp;
use ployz_decide::service_floor::{self, Raise};
use stateright::{Model, Property};

use crate::harness::Variant;

/// Minutes past the model's epoch.
type Minute = u8;

fn time(minute: Minute) -> Timestamp {
    Timestamp::from_millis(1_800_000_000_000 + u64::from(minute) * 60_000).expect("a safe time")
}

/// The clock stops here, so the forget signed at 2 is never ahead of it.
const LAST_MINUTE: Minute = 2;
const HONEST_FORGET: Minute = 2;
const AHEAD_BY: Minute = 11;

/// A new Container's approval.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Approval {
    V1,
    V2,
    R1,
    R2,
    W,
}

const APPROVALS: [Approval; 5] = [
    Approval::V1,
    Approval::V2,
    Approval::R1,
    Approval::R2,
    Approval::W,
];

impl Approval {
    const fn at(self) -> Minute {
        match self {
            Self::V1 | Self::R1 | Self::W => 1,
            Self::V2 => 2,
            Self::R2 => 3,
        }
    }

    const fn sibling(self) -> bool {
        matches!(self, Self::W)
    }

    const fn verified_rule(self) -> bool {
        matches!(self, Self::R1)
    }

    const fn index(self) -> usize {
        self as usize
    }
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Phase {
    Idle,
    /// The first floor read passed.
    Checked,
    /// The image is pulled; `still_holds` is next.
    Pulled,
    Created,
    Refused,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Kept {
    K1,
    Kr,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct State {
    clock: Minute,
    /// Persisted.
    floor: Option<Minute>,
    b_trusted: bool,
    admissions: [Phase; 5],
    started: [Option<bool>; 2],
    honest_forgets: u8,
    ahead_forgets: u8,
    crashed: bool,
    /// Ghost: the signed time of every forget the Machine took.
    taken: Vec<Minute>,
    /// Ghost: the highest floor so far.
    highest: Option<Minute>,
    /// Ghost: a create or start passed its last read under an approval that read forgot.
    used_below: bool,
    /// Ghost: an admission passed its first read, and pulled, under an approval that read
    /// forgot.
    pulled_below: bool,
    /// Ghost: a forget past the guard was refused.
    ahead_refused: bool,
    /// Ghost: a check refused an admission whose first read passed.
    recheck_refused: bool,
    /// Ghost: each approval's create passed while `app/api` had a floor.
    created_over_floor: [bool; 5],
}

impl State {
    fn phase(&self, approval: Approval) -> Phase {
        *self
            .admissions
            .get(approval.index())
            .expect("an approval slot")
    }

    fn phase_mut(&mut self, approval: Approval) -> &mut Phase {
        self.admissions
            .get_mut(approval.index())
            .expect("an approval slot")
    }

    fn start_mut(&mut self, kept: Kept) -> &mut Option<bool> {
        self.started.get_mut(kept as usize).expect("a kept slot")
    }
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Action {
    Forget,
    ForgetAhead,
    Check(Approval),
    Pull(Approval),
    Create(Approval),
    Start(Kept),
    RemoveB,
    Tick,
    Crash,
}

/// One change to the code as it ships, each a guard this model shows is needed.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Mutation {
    /// `still_holds` and `Standing::admits` skip the floor.
    DropRecheck,
    /// `rule_floor` runs only for a build checked online, not for a verified build.
    RuleFloorOnlyOnline,
    /// A kept admission keeps one newest time for all its keys, not one per key.
    NewestPerKeys,
    /// A forget past the guard raises the floor to the guard instead of being refused.
    Clamp,
    /// The admission of a version marked to forget leaves the floor to the forget route.
    NoRaiseAtAdmission,
    /// An approval signed at the floor counts as forgotten.
    FloorInclusive,
    /// A rule is refused at every read once its Service has a floor, approved again or not.
    RulesRetiredForGood,
    /// A floor is kept per Cluster, so the sibling reads `app/api`'s.
    FloorPerCluster,
}

#[derive(Clone, Copy, Debug)]
struct ServiceFloor {
    variant: Variant<Mutation>,
}

impl ServiceFloor {
    fn has(self, mutation: Mutation) -> bool {
        self.variant.proposal() == Some(mutation)
    }

    /// The floor `approval`'s Service reads: the sibling has none.
    fn floor_for(self, state: &State, approval: Approval) -> Option<Timestamp> {
        if approval.sibling() && !self.has(Mutation::FloorPerCluster) {
            None
        } else {
            state.floor.map(time)
        }
    }

    /// Whether a mutation refuses what the shipped decision let through.
    fn over_refuses(self, approval: Approval, floor: Option<Timestamp>) -> bool {
        let Some(floor) = floor else { return false };
        (self.has(Mutation::FloorInclusive) && time(approval.at()) <= floor)
            || (self.has(Mutation::RulesRetiredForGood)
                && matches!(approval, Approval::R1 | Approval::R2))
    }

    /// The first floor read, as the admission code makes it: the floor it leaves, when it
    /// passes.
    fn first_read(self, state: &State, approval: Approval) -> Option<Option<Timestamp>> {
        let floor = self.floor_for(state, approval);
        let now = time(state.clock);
        let after = match approval {
            _ if approval.verified_rule() && self.has(Mutation::RuleFloorOnlyOnline) => Some(floor),
            Approval::R1 | Approval::R2 => service_floor::admits(time(approval.at()), floor, now)
                .ok()
                .map(|()| floor),
            Approval::V1 | Approval::V2 | Approval::W => {
                let signature = service_floor::Signature {
                    key: (),
                    at: time(approval.at()),
                    forget: approval == Approval::V2 && !self.has(Mutation::NoRaiseAtAdmission),
                };
                service_floor::admit(&[signature], floor, now)
                    .ok()
                    .map(|admitted| admitted.floor)
            }
        };
        after.filter(|after| !self.over_refuses(approval, *after))
    }

    /// `Standing::admits` for a new Container's approval.
    fn recheck(self, state: &State, approval: Approval) -> bool {
        if self.has(Mutation::DropRecheck) {
            return true;
        }
        let floor = self.floor_for(state, approval);
        service_floor::keys_stand([(true, time(approval.at()))], floor)
            && !self.over_refuses(approval, floor)
    }

    /// `Standing::admits` for a kept Container.
    fn standing(self, state: &State, kept: Kept) -> bool {
        let floor = if self.has(Mutation::DropRecheck) {
            None
        } else {
            state.floor.map(time)
        };
        match kept {
            Kept::K1 if self.has(Mutation::NewestPerKeys) => {
                service_floor::keys_stand([(true, time(3)), (state.b_trusted, time(3))], floor)
            }
            Kept::K1 => {
                service_floor::keys_stand([(true, time(1)), (state.b_trusted, time(3))], floor)
            }
            Kept::Kr => service_floor::keys_stand([(true, time(1))], floor),
        }
    }

    /// The truth a read is held to: a trusted key, or the rule, at or past the floor.
    fn truly_stands(state: &State, minutes: &[(bool, Minute)]) -> bool {
        minutes
            .iter()
            .any(|(trusted, at)| *trusted && state.floor.is_none_or(|floor| *at >= floor))
    }

    fn truth_of(state: &State, approval: Approval) -> bool {
        approval.sibling() || Self::truly_stands(state, &[(true, approval.at())])
    }

    fn take(self, state: &mut State, signed: Minute) {
        let raised = if self.has(Mutation::Clamp) {
            Ok(Raise::Raised(time(signed.min(state.clock + 10))))
        } else {
            service_floor::raise(state.floor.map(time), time(signed), time(state.clock))
        };
        match raised {
            Ok(Raise::Raised(floor)) => {
                state.taken.push(signed);
                state.floor = Some(minute(floor));
            }
            Ok(Raise::Held(_)) => state.taken.push(signed),
            Err(_) => state.ahead_refused = true,
        }
        state.highest = state.highest.max(state.floor);
    }
}

impl Model for ServiceFloor {
    type State = State;
    type Action = Action;

    fn init_states(&self) -> Vec<State> {
        vec![State {
            clock: 0,
            floor: None,
            b_trusted: true,
            admissions: [Phase::Idle; 5],
            started: [None; 2],
            honest_forgets: 0,
            ahead_forgets: 0,
            crashed: false,
            taken: Vec::new(),
            highest: None,
            used_below: false,
            pulled_below: false,
            ahead_refused: false,
            recheck_refused: false,
            created_over_floor: [false; 5],
        }]
    }

    fn actions(&self, state: &State, actions: &mut Vec<Action>) {
        if state.honest_forgets < 2 {
            actions.push(Action::Forget);
        }
        if state.ahead_forgets < 1 {
            actions.push(Action::ForgetAhead);
        }
        for approval in APPROVALS {
            match state.phase(approval) {
                Phase::Idle => actions.push(Action::Check(approval)),
                Phase::Checked => actions.push(Action::Pull(approval)),
                Phase::Pulled => actions.push(Action::Create(approval)),
                Phase::Created | Phase::Refused => {}
            }
        }
        for (kept, started) in [Kept::K1, Kept::Kr].into_iter().zip(state.started) {
            if started.is_none() {
                actions.push(Action::Start(kept));
            }
        }
        if state.b_trusted {
            actions.push(Action::RemoveB);
        }
        if state.clock < LAST_MINUTE {
            actions.push(Action::Tick);
        }
        if !state.crashed {
            actions.push(Action::Crash);
        }
    }

    fn next_state(&self, last: &State, action: Action) -> Option<State> {
        let mut state = last.clone();
        match action {
            Action::Forget => {
                state.honest_forgets += 1;
                self.take(&mut state, HONEST_FORGET);
            }
            Action::ForgetAhead => {
                state.ahead_forgets += 1;
                let signed = state.clock + AHEAD_BY;
                self.take(&mut state, signed);
            }
            Action::Check(approval) => {
                let read = self.first_read(&state, approval);
                if let Some(Some(floor)) = read
                    && !approval.sibling()
                    && state.floor.map(time) != Some(floor)
                {
                    state.taken.push(approval.at());
                    state.floor = Some(minute(floor));
                    state.highest = state.highest.max(state.floor);
                }
                let passed = read.is_some();
                state.pulled_below |= passed && !Self::truth_of(&state, approval);
                *state.phase_mut(approval) = if passed {
                    Phase::Checked
                } else {
                    Phase::Refused
                };
            }
            Action::Pull(approval) => *state.phase_mut(approval) = Phase::Pulled,
            Action::Create(approval) => {
                let passed = self.recheck(&state, approval);
                state.used_below |= passed && !Self::truth_of(&state, approval);
                state.recheck_refused |= !passed;
                if passed && state.floor.is_some() {
                    *state
                        .created_over_floor
                        .get_mut(approval.index())
                        .expect("an approval slot") = true;
                }
                *state.phase_mut(approval) = if passed {
                    Phase::Created
                } else {
                    Phase::Refused
                };
            }
            Action::Start(kept) => {
                let passed = self.standing(&state, kept);
                let truth = match kept {
                    Kept::K1 => Self::truly_stands(&state, &[(true, 1), (state.b_trusted, 3)]),
                    Kept::Kr => Self::truly_stands(&state, &[(true, 1)]),
                };
                state.used_below |= passed && !truth;
                *state.start_mut(kept) = Some(passed);
            }
            Action::RemoveB => state.b_trusted = false,
            Action::Tick => state.clock += 1,
            Action::Crash => {
                state.crashed = true;
                for phase in &mut state.admissions {
                    if matches!(phase, Phase::Checked | Phase::Pulled) {
                        *phase = Phase::Idle;
                    }
                }
            }
        }
        Some(state)
    }

    fn properties(&self) -> Vec<Property<Self>> {
        vec![
            Property::always(NO_USE_BELOW, |_, state: &State| !state.used_below),
            Property::always(NO_PULL_BELOW, |_, state: &State| !state.pulled_below),
            Property::always(FLOOR_SIGNED, |_, state: &State| {
                state.floor.is_none_or(|floor| state.taken.contains(&floor))
            }),
            Property::always(FLOOR_RISES, |_, state: &State| state.floor >= state.highest),
            Property::always(FORGET_RAISES, |_, state: &State| {
                state.phase(Approval::V2) == Phase::Idle
                    || state.phase(Approval::V2) == Phase::Refused
                    || state.floor >= Some(Approval::V2.at())
            }),
            Property::sometimes(OWN_VERSION_CREATES, |_, state: &State| {
                created_over_floor(state, Approval::V2)
            }),
            Property::sometimes(RULE_AGAIN_CREATES, |_, state: &State| {
                created_over_floor(state, Approval::R2)
            }),
            Property::sometimes(SIBLING_CREATES, |_, state: &State| {
                created_over_floor(state, Approval::W)
            }),
            Property::sometimes(
                "an admission that passed its first read is refused at the recheck",
                |_, state: &State| state.recheck_refused,
            ),
            Property::sometimes(
                "a forget signed eleven minutes ahead is refused",
                |_, state: &State| state.ahead_refused,
            ),
            Property::sometimes(
                "a kept Container whose post-floor signer was removed is refused",
                |_, state: &State| !state.b_trusted && state.started.first() == Some(&Some(false)),
            ),
        ]
    }
}

/// Whether `approval`'s create passed while the floor was set, not merely before a
/// forget that came later.
fn created_over_floor(state: &State, approval: Approval) -> bool {
    state
        .created_over_floor
        .get(approval.index())
        .copied()
        .unwrap_or(false)
}

fn minute(at: Timestamp) -> Minute {
    let minute = (at.as_millis() - time(0).as_millis()) / 60_000;
    Minute::try_from(minute).expect("a model minute")
}

const NO_USE_BELOW: &str =
    "no create or Operator start passes its last floor read under an approval that floor forgot";
const NO_PULL_BELOW: &str = "no admission pulls after a first floor read that forgot its approval";
const FLOOR_SIGNED: &str = "the floor is always the signed time of a forget this Machine took";
const FLOOR_RISES: &str = "the floor never falls";
const FORGET_RAISES: &str =
    "an admitted version marked to forget has raised the floor to its own time";
const OWN_VERSION_CREATES: &str = "the forget's own version creates over the floor it raised";
const RULE_AGAIN_CREATES: &str = "a rule approved again after the forget creates over the floor";
const SIBLING_CREATES: &str = "the sibling Service, at no floor, creates while app/api has one";

fn story(path: &[(State, Option<Action>)]) -> String {
    let mut story = String::new();
    let mut led_by: Option<Action> = None;
    for (n, (state, action)) in path.iter().enumerate() {
        let what = led_by.map_or_else(|| "start".to_owned(), reads_as);
        led_by = *action;
        let _ = writeln!(
            story,
            "{n:2}. {what}\n    clock={} floor={:?} b_trusted={} admissions={:?} started={:?}",
            state.clock, state.floor, state.b_trusted, state.admissions, state.started
        );
    }
    story
}

fn reads_as(action: Action) -> String {
    match action {
        Action::Forget => format!("forget_versions: raise to {HONEST_FORGET}"),
        Action::ForgetAhead => format!("forget_versions: raise to the clock plus {AHEAD_BY}"),
        Action::Check(approval) => format!("{approval:?}: the first floor read"),
        Action::Pull(approval) => format!("{approval:?}: the image pull"),
        Action::Create(approval) => format!("{approval:?}: still_holds, then Docker creates"),
        Action::Start(kept) => format!("{kept:?}: start_checked reads Standing::admits"),
        Action::RemoveB => "key B is removed".to_owned(),
        Action::Tick => "the clock ticks".to_owned(),
        Action::Crash => "the daemon crashes; the floor stays".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::{Narrated, breaks, check, first_break};
    use stateright::Checker;

    impl Narrated for ServiceFloor {
        fn story(&self, path: &[(State, Option<Action>)]) -> String {
            story(path)
        }
    }

    fn mutated(mutation: Mutation) -> ServiceFloor {
        ServiceFloor {
            variant: Variant::Proposed(mutation),
        }
    }

    /// Every interleaving of the forgets, the admissions, the kept starts, a key removal,
    /// the clock and a crash keeps every property, and every `sometimes` is reached.
    #[test]
    fn no_approval_below_the_floor_is_used_once_the_floor_is_read() {
        let checker = check(ServiceFloor {
            variant: Variant::Shipped,
        });
        println!("as shipped: {} states", checker.unique_state_count());
        checker.assert_properties();
    }

    /// Without the floor in `still_holds`, V1 passes its first read before the forget and
    /// is created after it.
    #[test]
    fn the_recheck_reads_the_floor() {
        breaks(mutated(Mutation::DropRecheck), NO_USE_BELOW);
    }

    /// A verified build of R1 that skips the floor pulls after the forget. The recheck
    /// still refuses its create, so this guard spares only the pull.
    #[test]
    fn a_verified_build_reads_the_floor_too() {
        breaks(mutated(Mutation::RuleFloorOnlyOnline), NO_PULL_BELOW);
    }

    /// With one newest time for both keys, K1 starts under key A, which signed before the
    /// floor, once key B, which signed after it, is removed.
    #[test]
    fn each_key_keeps_its_own_time() {
        breaks(mutated(Mutation::NewestPerKeys), NO_USE_BELOW);
    }

    /// A clamp raises the floor to a time no forget was signed at, and each resend would
    /// raise it again.
    #[test]
    fn a_forget_ahead_is_refused_not_clamped() {
        breaks(mutated(Mutation::Clamp), FLOOR_SIGNED);
    }

    /// When only the forget route raises the floor, V2 is admitted while V1 still could be.
    #[test]
    fn a_deploy_marked_to_forget_raises_the_floor_as_it_is_admitted() {
        breaks(mutated(Mutation::NoRaiseAtAdmission), FORGET_RAISES);
    }

    fn unreached(mutation: Mutation, witness: &'static str) {
        assert!(
            first_break(mutated(mutation), witness, |_| true, false).is_none(),
            "{witness} is reached under {mutation:?}"
        );
    }

    /// Each witness of a creation after the forget is unreached once the decision it
    /// shows refuses too much, so none is met by a creation before the forget.
    #[test]
    fn each_creation_after_the_forget_needs_the_decision_it_witnesses() {
        unreached(Mutation::FloorInclusive, OWN_VERSION_CREATES);
        unreached(Mutation::RulesRetiredForGood, RULE_AGAIN_CREATES);
        unreached(Mutation::FloorPerCluster, SIBLING_CREATES);
    }
}
