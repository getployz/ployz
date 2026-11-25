//! One piece of work a management key started on a Machine (an exec, a tunnel, a Build or a
//! Build Grant's push) as `ployzd`'s registry of bound work keeps it
//! (`machine/local_machine/bound.rs`), against the key changes that end it.
//!
//! The work acts from its bind: its process runs, or its push is in flight. It holds a Bound
//! while it acts, takes another through its Tie under the registry's lock (`Tie::hold`), and
//! lets each go with evidence its work ended (`Bound::stopped`), with a doubt it reports
//! (`Bound::unconfirmed`, with or without a recheck), or silently (a Bound dropped). Its
//! process may stop at any time, seen or not. A key change removes the key, then
//! `BoundWork::end_unheld` ends the work, waits until every Bound went or its deadline
//! passed, runs each doubt's recheck, and reports the work while it is unconfirmed. A second
//! key change does the same.
//!
//! The registry's facts about the piece are `ployz_decide::bound_work::Entry`, called here.
//!
//! The property: a key change that reports nothing leaves the removed key's work acting
//! nowhere. Once ended, the work takes no Bound again, so it never acts after that.
//!
//! `Broken` replays the registry of round 1 (de1319aef3): the last Tie or Bound going
//! removed the entry with its causes (`untie_forgets`, its `Binding` drop), a key change
//! took every entry it ended out of the registry before it waited (`end_extracts`, its
//! `extract_if`), and a Bound dropped without a report counted as stopped
//! (`drop_is_evidence`).
//!
//! Out of scope: more than one piece, more than two Bounds at once, three taken or two key
//! changes, a recheck
//! that runs while Bounds still go, and a key change that fails before it ends work.

use std::fmt::Write as _;

use ployz_decide::bound_work::{Entry, Release};
use stateright::{Model, Property};

/// A doubt a Bound left: whether a later look can clear it, as Docker can for an exec and
/// the build slot can for a Build, and Image Ingest can't for a manifest it never answered.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct Doubt {
    recheck: bool,
}

/// How a Bound goes.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum How {
    /// `Bound::stopped`, by a holder that saw the work end.
    Ended,
    /// `Bound::unconfirmed`, with a recheck or without.
    Reported { recheck: bool },
    /// The Bound dropped: its task panicked or was aborted.
    Dropped,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Action {
    /// `Tie::hold`, which acts at once: a commit sent, a process spawned.
    Hold,
    /// The process stops or the push lands, whether or not anyone sees it.
    Stop,
    Release(How),
    /// The work's own Tie goes.
    Untie,
    /// A key change writes its record and ends the work.
    Change,
    /// The key change's wait ends, at its deadline or once every Bound went: it rechecks,
    /// reports and settles.
    Finish {
        timed_out: bool,
    },
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct State {
    /// The registry's entry, or `None` once it forgot the work.
    entry: Option<Entry<Doubt>>,
    /// Round 1: a key change took the entry out of the registry, and still reports it.
    extracted: bool,
    acting: bool,
    bounds: u8,
    /// Bounds the work ever took, its first included.
    taken: u8,
    tie_held: bool,
    changes: u8,
    changing: bool,
    /// A key change returned reporting nothing.
    clean: bool,
    /// A key change reported the work.
    reported: bool,
    /// A Bound left a doubt before any key change ended the work.
    doubted_live: bool,
}

/// Round 1's registry, one flaw at a time.
#[derive(Clone, Copy, Debug, Default, Hash, PartialEq, Eq)]
struct Broken {
    untie_forgets: bool,
    end_extracts: bool,
    drop_is_evidence: bool,
}

#[derive(Clone, Copy, Debug, Default)]
struct BoundWork {
    broken: Broken,
}

const MAX_BOUNDS: u8 = 2;
const MAX_TAKEN: u8 = 3;
const MAX_CHANGES: u8 = 2;
const CLEAN_MEANS_STOPPED: &str =
    "a key change that reports nothing leaves its work acting nowhere";

impl BoundWork {
    /// The registry drops the entry when nothing keeps it: once settled, or in round 1 once
    /// the last Tie and Bound went.
    fn settle(self, state: &mut State) {
        let Some(entry) = &state.entry else { return };
        let unreferenced = state.bounds == 0 && !state.tie_held;
        let gone = if self.broken.untie_forgets {
            unreferenced && !state.extracted
        } else {
            entry.settled()
        };
        if gone {
            state.entry = None;
        }
    }
}

impl Model for BoundWork {
    type State = State;
    type Action = Action;

    fn init_states(&self) -> Vec<State> {
        vec![State {
            entry: Some(Entry::bound()),
            extracted: false,
            acting: true,
            bounds: 1,
            taken: 1,
            tie_held: true,
            changes: 0,
            changing: false,
            clean: false,
            reported: false,
            doubted_live: false,
        }]
    }

    fn actions(&self, state: &State, actions: &mut Vec<Action>) {
        if (state.tie_held || state.bounds > 0)
            && state.bounds < MAX_BOUNDS
            && state.taken < MAX_TAKEN
        {
            actions.push(Action::Hold);
        }
        if state.acting {
            actions.push(Action::Stop);
        }
        if state.bounds > 0 {
            if !state.acting {
                actions.push(Action::Release(How::Ended));
            }
            actions.extend([
                Action::Release(How::Reported { recheck: true }),
                Action::Release(How::Reported { recheck: false }),
                Action::Release(How::Dropped),
            ]);
        }
        if state.tie_held {
            actions.push(Action::Untie);
        }
        if !state.changing && state.changes < MAX_CHANGES {
            actions.push(Action::Change);
        }
        if state.changing {
            actions.push(Action::Finish { timed_out: true });
            if state.bounds == 0 {
                actions.push(Action::Finish { timed_out: false });
            }
        }
    }

    fn next_state(&self, last: &State, action: Action) -> Option<State> {
        let mut state = last.clone();
        match action {
            Action::Hold => {
                // Round 1 refused a Tie whose end token a key change cancelled, too.
                state.taken += 1;
                if state.entry.as_mut().is_some_and(Entry::hold) {
                    state.bounds += 1;
                    state.acting = true;
                }
            }
            Action::Stop => state.acting = false,
            Action::Release(how) => {
                state.bounds -= 1;
                let doubt = match how {
                    How::Ended => None,
                    How::Reported { recheck } => Some(Doubt { recheck }),
                    How::Dropped if self.broken.drop_is_evidence => None,
                    How::Dropped => Some(Doubt { recheck: false }),
                };
                if let Some(entry) = &mut state.entry {
                    if doubt.is_some() && !entry.is_ended() {
                        state.doubted_live = true;
                    }
                    entry.release(doubt.map_or(Release::Ended, Release::Unconfirmed));
                    if state.bounds == 0 && !state.tie_held {
                        entry.untie();
                    }
                }
                self.settle(&mut state);
            }
            Action::Untie => {
                state.tie_held = false;
                if state.bounds == 0
                    && let Some(entry) = &mut state.entry
                {
                    entry.untie();
                }
                self.settle(&mut state);
            }
            Action::Change => {
                state.changing = true;
                state.changes += 1;
                if let Some(entry) = &mut state.entry {
                    entry.end();
                    state.extracted = self.broken.end_extracts;
                }
            }
            Action::Finish { timed_out } => {
                state.changing = false;
                let acting = state.acting;
                let reported = state.entry.as_mut().is_some_and(|entry| {
                    if self.broken.end_extracts {
                        // Round 1 looked at nothing again: what the Bounds said, and the
                        // deadline.
                        !entry.doubts().is_empty() || timed_out && entry.holders() > 0
                    } else {
                        entry.confirm(|doubt| doubt.recheck && !acting);
                        entry.unconfirmed()
                    }
                });
                if state.extracted {
                    state.entry = None;
                    state.extracted = false;
                }
                self.settle(&mut state);
                if reported {
                    state.reported = true;
                } else {
                    state.clean = true;
                }
            }
        }
        Some(state)
    }

    fn properties(&self) -> Vec<Property<Self>> {
        let mut properties = vec![
            Property::always(CLEAN_MEANS_STOPPED, |_, state: &State| {
                !(state.clean && state.acting)
            }),
            Property::sometimes(
                "a key change waits for the work to stop and reports nothing",
                |_, state: &State| state.clean && !state.reported,
            ),
            Property::sometimes(
                "a key change reports work that still acts",
                |_, state: &State| state.reported && state.acting,
            ),
            Property::sometimes(
                "a later key change clears what an earlier one reported",
                |_, state: &State| state.reported && state.clean && state.entry.is_none(),
            ),
        ];
        if self.broken == Broken::default() {
            properties.push(Property::sometimes(
                "a key change reports a doubt left before it",
                |_, state: &State| state.doubted_live && state.reported,
            ));
        }
        properties
    }
}

impl crate::harness::Narrated for BoundWork {
    fn story(&self, path: &[(State, Option<Action>)]) -> String {
        let mut story = String::new();
        for (n, (state, action)) in path.iter().enumerate() {
            let _ = writeln!(
                story,
                "{n:2}. entry={:?} extracted={} acting={} bounds={} tie={} changes={} clean={} reported={}{}",
                state.entry,
                state.extracted,
                state.acting,
                state.bounds,
                state.tie_held,
                state.changes,
                state.clean,
                state.reported,
                action.map_or_else(String::new, |action| format!("\n    then {action:?}")),
            );
        }
        story
    }
}

#[cfg(test)]
mod tests {
    use stateright::Checker as _;

    use super::*;
    use crate::harness::{breaks, check};

    /// The first path on which round 1's `broken` registry says a key change stopped work
    /// that still acts.
    fn round_one(broken: Broken) -> State {
        let path = breaks(BoundWork { broken }, CLEAN_MEANS_STOPPED);
        let (last, _) = path.last().expect("a path").clone();
        assert!(last.clean && last.acting);
        last
    }

    /// The registry keeps the entry until evidence settles it, reports it to every key
    /// change while unconfirmed, and clears it only once a recheck sees the work stopped.
    #[test]
    fn a_clean_key_change_leaves_no_work_acting() {
        let checker = check(BoundWork::default());
        println!("as shipped: {} states", checker.unique_state_count());
        checker.assert_properties();
    }

    /// A Build ends Unknown while its key holds Full, and its Tie goes with it: round 1's
    /// registry forgot the doubt with the entry, so removing the key reported nothing while
    /// its builder ran on.
    #[test]
    fn the_last_tie_going_must_not_take_a_doubt_with_it() {
        let last = round_one(Broken {
            untie_forgets: true,
            ..Broken::default()
        });
        assert!(
            last.doubted_live,
            "a Bound left a doubt before the key change"
        );
    }

    /// A key change times out on work still acting and reports it; round 1 had taken the
    /// entry out of the registry, so the next key change found nothing and reported clean.
    #[test]
    fn a_key_change_must_leave_unconfirmed_work_for_the_next() {
        let last = round_one(Broken {
            end_extracts: true,
            ..Broken::default()
        });
        assert!(last.reported, "an earlier key change reported the work");
    }

    /// A Bound whose task was aborted before it stopped its process counted as stopped in
    /// round 1, so the key change waited for nothing and reported clean.
    #[test]
    fn a_dropped_bound_is_no_evidence_its_work_ended() {
        round_one(Broken {
            drop_is_evidence: true,
            ..Broken::default()
        });
    }
}
