//! E0 scratch model, throwaway. One approved Move of a Volume from A to B, driven by a
//! hostile Cloud that may send any request at any time, in any order, any number of
//! times, withhold any request, keep every receipt it ever saw, and replay a receipt
//! from an earlier Move. A third Machine C is a mesh peer the approval does not name.
//!
//! The model sits above `ployz_core::plan`: a Machine's copy is reduced to what the
//! incompatible outcomes need (who writes, from which snapshot). Each design is a set
//! of local checks and persisted records on A and B. A daemon may crash between a
//! receipt leaving and its record reaching disk only where a `Broken` flag lets the
//! code order them that way; otherwise a decision is persisted before its receipt or
//! effect leaves the Machine.
//!
//! Designs:
//! - `Envelope`: the A2 authorization alone. Each Machine admits each step against the
//!   approved transition graph and consumes it, with no receipt from the other side.
//! - `TargetDecides`: B holds the commit point. B persists `Committed` or `Aborted`
//!   once. B promotes only with A's signed cut receipt for G; A resumes only with B's
//!   signed abort receipt.
//! - `SourceDecides`: A holds the commit point. After its cut, A persists `Handed` or
//!   `Resumed` once. B promotes only with A's signed handoff receipt for G. A resumes on
//!   its own record and needs nothing from B.

use std::collections::{HashSet, VecDeque};

use stateright::{Checker, HasDiscoveries, Model, Property};

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Design {
    Envelope,
    TargetDecides,
    SourceDecides,
}

#[derive(Clone, Copy, Debug, Default, Hash, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)]
struct Broken {
    /// The deciding Machine sends its receipt before its decision reaches disk.
    persist_after_receipt: bool,
    /// The verifier ignores the receipt's operation ID, so an earlier Move's receipt counts.
    unbound_op: bool,
    /// The receipt names no snapshot, so B may promote at any snapshot it holds.
    unbound_snapshot: bool,
    /// The send stream serves any mesh peer, as `volume_send.rs` does today.
    open_stream: bool,
    /// The deciding Machine keeps no decision record, so it may decide twice.
    no_decision_record: bool,
    /// B's abort is an admission, so it is refused once admission has expired.
    abort_needs_admission: bool,
    /// B treats a pull or promotion after expiry as new work even with A's handoff
    /// receipt in hand, instead of as completion of an op A already committed.
    no_completion_admission: bool,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq, PartialOrd, Ord)]
enum Snap {
    /// A warm round's snapshot. A wrote past it before the cut.
    Warm,
    /// The cut's snapshot G. It holds every write A acknowledged.
    Final,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Base {
    /// A's original Writer, ahead of every snapshot.
    Live,
    At(Snap),
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Copy {
    None,
    Writer(Base),
    Slot(Snap),
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Decision {
    /// A: handed the Volume to B at G. B: committed.
    Forward,
    /// A: resumed as Writer. B: aborted.
    Back,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Receipt {
    None,
    /// A's signed cut receipt for this op at G (`TargetDecides`).
    Cut,
    /// A's signed handoff receipt for this op to B at G (`SourceDecides`).
    Hand,
    /// A receipt A signed in an earlier Move of this Volume, at the warm snapshot.
    Old,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
#[allow(clippy::enum_variant_names)]
enum Action {
    CutA,
    PullB(Snap),
    PullC,
    HandA,
    ResumeA,
    AbortB,
    PromoteB(Snap, Receipt),
    /// The daemon writes a decision whose receipt already left (`persist_after_receipt`).
    Persist(Who),
    Crash(Who),
    Expire,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Who {
    A,
    B,
}

#[derive(Clone, Debug, Default, Hash, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)]
struct Bag {
    cut: bool,
    hand: bool,
    abort: bool,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)]
struct State {
    a: Copy,
    b: Copy,
    c_has: bool,
    /// A's persisted records: its cut of this op, and its decision.
    a_cut: bool,
    a_decision: Option<Decision>,
    /// A decision whose receipt left but which is not yet on disk; a crash loses it.
    a_pending: bool,
    b_decision: Option<Decision>,
    b_pending: bool,
    admitted_a: bool,
    admitted_b: bool,
    expired: bool,
    bag: Bag,
    crashes: u8,
    ever_resumed: bool,
    ever_promoted: bool,
}

#[derive(Clone, Debug)]
struct HostileMove {
    design: Design,
    broken: Broken,
    crashes: u8,
}

impl HostileMove {
    fn new(design: Design) -> Self {
        Self {
            design,
            broken: Broken::default(),
            crashes: 1,
        }
    }

    fn admit(&self, state: &mut State, who: Who) -> bool {
        // A handoff receipt shows A committed this op forward, so B's part is completion.
        let completing = who == Who::B
            && self.design == Design::SourceDecides
            && state.bag.hand
            && !self.broken.no_completion_admission;
        let admitted = match who {
            Who::A => &mut state.admitted_a,
            Who::B => &mut state.admitted_b,
        };
        if *admitted {
            return true;
        }
        if state.expired && !completing {
            return false;
        }
        *admitted = true;
        true
    }

    /// A serves snapshot `snap` of this op only if it holds it.
    fn serves(state: &State, snap: Snap) -> bool {
        match snap {
            Snap::Warm => true,
            Snap::Final => state.a_cut,
        }
    }

    /// The receipt check B makes before promoting at `at`.
    fn receipt_lets_b_promote(&self, state: &State, at: Snap, with: Receipt) -> bool {
        let wanted = match self.design {
            Design::Envelope => return true,
            Design::TargetDecides => Receipt::Cut,
            Design::SourceDecides => Receipt::Hand,
        };
        let (held, bound_at) = match with {
            Receipt::Cut if wanted == Receipt::Cut => (state.bag.cut, Snap::Final),
            Receipt::Hand if wanted == Receipt::Hand => (state.bag.hand, Snap::Final),
            Receipt::Old => (self.broken.unbound_op, Snap::Warm),
            Receipt::None | Receipt::Cut | Receipt::Hand => return false,
        };
        held && (self.broken.unbound_snapshot || at == bound_at)
    }

    #[allow(clippy::too_many_lines)]
    fn apply(&self, last: &State, action: Action) -> Option<State> {
        let mut s = last.clone();
        match action {
            Action::CutA => {
                if s.a_cut {
                    // Re-issue the cut receipt from A's record; no effect.
                    if s.bag.cut {
                        return None;
                    }
                    s.bag.cut = true;
                    return Some(s);
                }
                if s.a != Copy::Writer(Base::Live) || !self.admit(&mut s, Who::A) {
                    return None;
                }
                s.a = Copy::Slot(Snap::Final);
                s.a_cut = true;
                s.bag.cut = true;
            }
            Action::PullB(snap) => {
                if matches!(s.b, Copy::Writer(_)) || !Self::serves(&s, snap) {
                    return None;
                }
                if let Copy::Slot(have) = s.b
                    && have >= snap
                {
                    return None;
                }
                if !self.admit(&mut s, Who::B) {
                    return None;
                }
                s.b = Copy::Slot(snap);
            }
            Action::PullC => {
                if !self.broken.open_stream || s.c_has {
                    return None;
                }
                s.c_has = true;
            }
            Action::HandA => {
                if self.design != Design::SourceDecides || s.a != Copy::Slot(Snap::Final) {
                    return None;
                }
                match s.a_decision {
                    Some(Decision::Forward) => {
                        if s.bag.hand {
                            return None;
                        }
                        s.bag.hand = true;
                        return Some(s);
                    }
                    Some(Decision::Back) if !self.broken.no_decision_record => return None,
                    Some(Decision::Back) | None => {}
                }
                if s.a_pending || !self.admit(&mut s, Who::A) {
                    return None;
                }
                s.bag.hand = true;
                if self.broken.persist_after_receipt {
                    s.a_pending = true;
                } else if !self.broken.no_decision_record {
                    s.a_decision = Some(Decision::Forward);
                }
            }
            Action::ResumeA => {
                if s.a != Copy::Slot(Snap::Final) || !self.admit(&mut s, Who::A) {
                    return None;
                }
                let allowed = match self.design {
                    Design::Envelope => true,
                    Design::TargetDecides => s.bag.abort,
                    Design::SourceDecides => {
                        self.broken.no_decision_record
                            || (s.a_decision != Some(Decision::Forward) && !s.a_pending)
                    }
                };
                if !allowed {
                    return None;
                }
                if self.design == Design::SourceDecides && !self.broken.no_decision_record {
                    s.a_decision = Some(Decision::Back);
                }
                s.a = Copy::Writer(Base::At(Snap::Final));
                s.ever_resumed = true;
            }
            Action::AbortB => {
                if self.design != Design::TargetDecides {
                    return None;
                }
                match s.b_decision {
                    Some(Decision::Back) => {
                        if s.bag.abort {
                            return None;
                        }
                        s.bag.abort = true;
                        return Some(s);
                    }
                    Some(Decision::Forward) if !self.broken.no_decision_record => return None,
                    Some(Decision::Forward) | None => {}
                }
                if s.b_pending {
                    return None;
                }
                if self.broken.abort_needs_admission && !self.admit(&mut s, Who::B) {
                    return None;
                }
                s.bag.abort = true;
                if self.broken.persist_after_receipt {
                    s.b_pending = true;
                } else if !self.broken.no_decision_record {
                    s.b_decision = Some(Decision::Back);
                }
            }
            Action::PromoteB(at, with) => {
                if s.b != Copy::Slot(at)
                    || !self.receipt_lets_b_promote(&s, at, with)
                    || !self.admit(&mut s, Who::B)
                {
                    return None;
                }
                if self.design == Design::TargetDecides {
                    if !self.broken.no_decision_record
                        && (s.b_decision == Some(Decision::Back) || s.b_pending)
                    {
                        return None;
                    }
                    if !self.broken.no_decision_record {
                        s.b_decision = Some(Decision::Forward);
                    }
                }
                s.b = Copy::Writer(Base::At(at));
                s.ever_promoted = true;
            }
            Action::Persist(who) => {
                let pending = match who {
                    Who::A => &mut s.a_pending,
                    Who::B => &mut s.b_pending,
                };
                if !*pending {
                    return None;
                }
                *pending = false;
                match who {
                    Who::A => s.a_decision = Some(Decision::Forward),
                    Who::B => s.b_decision = Some(Decision::Back),
                }
            }
            Action::Crash(who) => {
                if s.crashes >= self.crashes {
                    return None;
                }
                let pending = match who {
                    Who::A => &mut s.a_pending,
                    Who::B => &mut s.b_pending,
                };
                if !*pending {
                    return None;
                }
                *pending = false;
                s.crashes += 1;
            }
            Action::Expire => {
                if s.expired {
                    return None;
                }
                s.expired = true;
            }
        }
        Some(s)
    }

    /// The honest driver that takes over once np1 is clean: admission has expired, it
    /// holds none of the old driver's receipts, and asks each Machine to re-issue from
    /// its records. `reach` says which Machines answer. It returns whether the Volume
    /// ends with exactly one Writer and no lost write.
    fn recover(&self, from: &State, reach_a: bool, reach_b: bool) -> bool {
        let mut s = from.clone();
        s.expired = true;
        s.bag = Bag::default();
        let step = |s: &mut State, action: Action| -> bool {
            let to_a = matches!(
                action,
                Action::CutA | Action::HandA | Action::ResumeA | Action::Persist(Who::A)
            );
            let to_b = matches!(
                action,
                Action::PullB(_) | Action::AbortB | Action::PromoteB(..) | Action::Persist(Who::B)
            );
            // A pull needs both ends.
            let needs_a = to_a || matches!(action, Action::PullB(_));
            if (needs_a && !reach_a) || (to_b && !reach_b) {
                return false;
            }
            match self.apply(s, action) {
                Some(next) => {
                    *s = next;
                    true
                }
                None => false,
            }
        };
        if s.a_pending && reach_a {
            step(&mut s, Action::Persist(Who::A));
        }
        if s.b_pending && reach_b {
            step(&mut s, Action::Persist(Who::B));
        }
        let writers = |s: &State| {
            [s.a, s.b]
                .iter()
                .filter(|copy| matches!(copy, Copy::Writer(_)))
                .count()
        };
        if writers(&s) == 0 {
            let forward = |s: &mut State, receipt: Receipt, reissue: Action| {
                // A receipt re-issued from A's record, or one B already used.
                let _ = step(s, reissue);
                if s.b != Copy::Slot(Snap::Final) && !step(s, Action::PullB(Snap::Final)) {
                    return;
                }
                let _ = step(s, Action::PromoteB(Snap::Final, receipt));
            };
            match self.design {
                Design::Envelope => {
                    if !step(&mut s, Action::ResumeA) {
                        forward(&mut s, Receipt::None, Action::Expire);
                    }
                }
                Design::TargetDecides => {
                    if s.b_decision == Some(Decision::Forward) {
                        forward(&mut s, Receipt::Cut, Action::CutA);
                    } else {
                        let _ = step(&mut s, Action::AbortB);
                        let _ = step(&mut s, Action::ResumeA);
                    }
                }
                Design::SourceDecides => {
                    if s.a_decision == Some(Decision::Forward) {
                        forward(&mut s, Receipt::Hand, Action::HandA);
                    } else {
                        let _ = step(&mut s, Action::ResumeA);
                    }
                }
            }
        }
        writers(&s) == 1 && s.b != Copy::Writer(Base::At(Snap::Warm))
    }

    fn reachable(&self) -> Vec<State> {
        let mut seen = HashSet::new();
        let mut queue: VecDeque<State> = self.init_states().into();
        let mut out = Vec::new();
        while let Some(state) = queue.pop_front() {
            if !seen.insert(state.clone()) {
                continue;
            }
            let mut actions = Vec::new();
            self.actions(&state, &mut actions);
            for action in actions {
                if let Some(next) = self.next_state(&state, action) {
                    queue.push_back(next);
                }
            }
            out.push(state);
        }
        out
    }
}

const TWO_WRITERS: &str = "never two Writers";
const BOTH_OUTCOMES: &str = "never both A resumed and B promoted";
const LOST_WRITES: &str = "B never writes from a snapshot older than the cut";
const RECIPIENT: &str = "no unapproved Machine receives the Volume";
const RECOVERABLE: &str = "a clean driver recovers to exactly one Writer";

impl Model for HostileMove {
    type State = State;
    type Action = Action;

    fn init_states(&self) -> Vec<State> {
        [Copy::None, Copy::Slot(Snap::Warm)]
            .into_iter()
            .map(|b| State {
                a: Copy::Writer(Base::Live),
                b,
                c_has: false,
                a_cut: false,
                a_decision: None,
                a_pending: false,
                b_decision: None,
                b_pending: false,
                admitted_a: false,
                admitted_b: false,
                expired: false,
                bag: Bag::default(),
                crashes: 0,
                ever_resumed: false,
                ever_promoted: false,
            })
            .collect()
    }

    fn actions(&self, _: &State, actions: &mut Vec<Action>) {
        actions.extend([
            Action::CutA,
            Action::PullB(Snap::Warm),
            Action::PullB(Snap::Final),
            Action::PullC,
            Action::HandA,
            Action::ResumeA,
            Action::AbortB,
            Action::Persist(Who::A),
            Action::Persist(Who::B),
            Action::Crash(Who::A),
            Action::Crash(Who::B),
            Action::Expire,
        ]);
        for at in [Snap::Warm, Snap::Final] {
            for with in [Receipt::None, Receipt::Cut, Receipt::Hand, Receipt::Old] {
                actions.push(Action::PromoteB(at, with));
            }
        }
    }

    fn next_state(&self, last: &State, action: Action) -> Option<State> {
        self.apply(last, action)
    }

    fn properties(&self) -> Vec<Property<Self>> {
        vec![
            Property::always(TWO_WRITERS, |_, s: &State| {
                !(matches!(s.a, Copy::Writer(_)) && matches!(s.b, Copy::Writer(_)))
            }),
            Property::always(BOTH_OUTCOMES, |_, s: &State| {
                !(s.ever_resumed && s.ever_promoted)
            }),
            Property::always(LOST_WRITES, |_, s: &State| {
                s.b != Copy::Writer(Base::At(Snap::Warm))
            }),
            Property::always(RECIPIENT, |_, s: &State| !s.c_has),
            Property::always(RECOVERABLE, |model: &Self, s: &State| {
                model.recover(s, true, true)
            }),
            Property::sometimes("forward", |_, s: &State| {
                s.b == Copy::Writer(Base::At(Snap::Final)) && s.a == Copy::Slot(Snap::Final)
            }),
            Property::sometimes("back", |_, s: &State| {
                s.ever_resumed && !matches!(s.b, Copy::Writer(_))
            }),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(model: HostileMove) -> impl Checker<HostileMove> {
        model.checker().spawn_bfs().join()
    }

    fn report(model: &HostileMove) {
        let checker = run(model.clone());
        println!(
            "{:?} {:?}: {} states",
            model.design,
            model.broken,
            checker.unique_state_count()
        );
        for name in [TWO_WRITERS, BOTH_OUTCOMES, LOST_WRITES, RECIPIENT, RECOVERABLE] {
            match checker.discovery(name) {
                Some(path) => println!("  FAILS {name}:\n{path}"),
                None => println!("  holds {name}"),
            }
        }
    }

    #[test]
    fn designs() {
        for design in [Design::Envelope, Design::TargetDecides, Design::SourceDecides] {
            report(&HostileMove::new(design));
        }
    }

    #[test]
    fn safe_designs_hold() {
        for design in [Design::TargetDecides, Design::SourceDecides] {
            run(HostileMove::new(design)).assert_properties();
        }
    }

    #[test]
    fn envelope_alone_gives_both_outcomes() {
        let checker = run(HostileMove::new(Design::Envelope));
        checker.assert_any_discovery(TWO_WRITERS);
        checker.assert_any_discovery(BOTH_OUTCOMES);
        checker.assert_any_discovery(LOST_WRITES);
    }

    fn breaks(design: Design, broken: Broken, properties: &[&'static str]) {
        let model = HostileMove {
            broken,
            ..HostileMove::new(design)
        };
        report(&model);
        let checker = model
            .checker()
            .finish_when(HasDiscoveries::AllOf(properties.iter().copied().collect()))
            .spawn_bfs()
            .join();
        for property in properties {
            checker.assert_any_discovery(property);
        }
    }

    #[test]
    fn each_guard_earns_its_keep() {
        let b = Broken::default;
        for design in [Design::SourceDecides, Design::TargetDecides] {
            breaks(
                design,
                Broken {
                    persist_after_receipt: true,
                    ..b()
                },
                &[TWO_WRITERS],
            );
            breaks(
                design,
                Broken {
                    no_decision_record: true,
                    ..b()
                },
                &[TWO_WRITERS],
            );
            breaks(
                design,
                Broken {
                    unbound_op: true,
                    ..b()
                },
                &[TWO_WRITERS, LOST_WRITES],
            );
            breaks(
                design,
                Broken {
                    unbound_snapshot: true,
                    ..b()
                },
                &[LOST_WRITES],
            );
            breaks(
                design,
                Broken {
                    open_stream: true,
                    ..b()
                },
                &[RECIPIENT],
            );
        }
        breaks(
            Design::TargetDecides,
            Broken {
                abort_needs_admission: true,
                ..b()
            },
            &[RECOVERABLE],
        );
        breaks(
            Design::SourceDecides,
            Broken {
                no_completion_admission: true,
                ..b()
            },
            &[RECOVERABLE],
        );
    }

    /// Where recovery still ends with one Writer when one participant never answers
    /// again. Counts reachable states with no Writer.
    #[test]
    fn recovery_with_a_participant_gone() {
        for design in [Design::Envelope, Design::TargetDecides, Design::SourceDecides] {
            let model = HostileMove::new(design);
            let stranded: Vec<State> = model
                .reachable()
                .into_iter()
                .filter(|s| !matches!(s.a, Copy::Writer(_)) && !matches!(s.b, Copy::Writer(_)))
                .collect();
            let without_b = stranded
                .iter()
                .filter(|s| model.recover(s, true, false))
                .count();
            let without_a = stranded
                .iter()
                .filter(|s| model.recover(s, false, true))
                .count();
            println!(
                "{design:?}: {} states with no Writer; {without_b} recover with B gone, {without_a} with A gone",
                stranded.len()
            );
        }
    }
}
