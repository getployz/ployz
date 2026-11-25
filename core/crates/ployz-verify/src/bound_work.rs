//! One piece of work a management key started on a Machine (an exec, a tunnel, a Build or a
//! Build Grant's push) as `ployzd`'s registry of bound work keeps it
//! (`machine/local_machine/bound.rs`), against the key changes that end it.
//!
//! The work starts pending: its start is sent, and Docker may run it at any time after,
//! outside every lock. It acts once started, and may stop at any time, seen or not. It holds
//! a Bound while it acts, takes another through its Tie under the registry's lock
//! (`Tie::hold`), which acts at once, and lets each go with what its holder last saw of the
//! work (`Bound::stopped`), with a doubt it reports (`Bound::unconfirmed`, with or without a
//! recheck), or silently (a Bound dropped). A holder may see what is true now, so it may
//! report the work stopped on a sighting that can also mean it hasn't started, its state was
//! lost, the look failed, or another holder had the lock. A key change removes the key, then
//! `BoundWork::end_unheld` ends the work, waits until every Bound went or its deadline
//! passed, runs each doubt's recheck, which sees what a look can see now, and reports the
//! work while it is unconfirmed. A second key change does the same.
//!
//! The registry's facts about the piece are `ployz_decide::bound_work::Entry`, and which
//! sighting is evidence the work ended is `ployz_decide::bound_work::Seen::ended`, both
//! called here. Which sightings a look can make while the work is pending, acting or done is
//! this model's own statement of the world, [`possible`].
//!
//! The property: a key change that reports nothing leaves the removed key's work acting
//! nowhere and unable to start. Once ended, the work takes no Bound again.
//!
//! `Broken` replays earlier registries one flaw at a time. Round 1 (de1319aef3): the last
//! Tie or Bound going removed the entry with its causes (`untie_forgets`), a key change took
//! every entry it ended out of the registry before it waited (`end_extracts`), and a Bound
//! dropped without a report counted as stopped (`drop_is_evidence`). Round 2 (90fce71b2b)
//! counted these sightings as an end: an exec Docker reported not running with no exit code
//! (`pending_ends`), an exec Docker answered 404 for (`forgotten_ends`), a `/proc` read that
//! failed (`unchecked_ends`), and a build slot another admission held (`busy_ends`).
//!
//! Out of scope: more than one piece, more than two Bounds at once, three taken or two key
//! changes, a recheck that runs while Bounds still go, and a key change that fails before it
//! ends work.

use std::fmt::Write as _;

use ployz_decide::bound_work::{Entry, Seen};
use stateright::{Model, Property};

/// A doubt a Bound left: whether a later look can clear it, as Docker can for an exec and
/// the build slot can for a Build.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct Doubt {
    recheck: bool,
}

/// Where the work itself is, whoever sees it.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Phase {
    /// Its start was sent, and Docker may still run it.
    Pending,
    Acting,
    Done,
}

/// Whether a look can see `seen` while the work is in `phase`: a sighting of an end only
/// once it ended, and absence any time.
const fn possible(seen: Seen, phase: Phase) -> bool {
    match seen {
        Seen::ExitCode
        | Seen::Reaped
        | Seen::Joined
        | Seen::ContainerGone
        | Seen::NeverSent
        | Seen::Answered
        | Seen::Landed => matches!(phase, Phase::Done),
        Seen::Pending | Seen::Forgotten | Seen::Unchecked | Seen::Busy => true,
    }
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Action {
    /// `Tie::hold`, which acts at once: a commit sent, a process spawned.
    Hold,
    /// Docker runs the command it was asked to start.
    Start,
    /// The process stops, its start fails, or the push lands, whether or not anyone sees it.
    Stop,
    /// `Bound::stopped`, its holder having last seen this.
    Stopped(Seen),
    /// `Bound::unconfirmed`, with a recheck or without.
    Unconfirmed { recheck: bool },
    /// The Bound dropped: its task panicked or was aborted.
    Dropped,
    /// The work's own Tie goes.
    Untie,
    /// A key change writes its record and ends the work.
    Change,
    /// The key change's wait ends, at its deadline or once every Bound went: its rechecks
    /// see `look`, then it reports and settles.
    Finish { timed_out: bool, look: Seen },
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct State {
    /// The registry's entry, or `None` once it forgot the work.
    entry: Option<Entry<Doubt>>,
    /// Round 1: a key change took the entry out of the registry, and still reports it.
    extracted: bool,
    phase: Phase,
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
    /// A holder reported the work stopped on a sighting that wasn't true.
    misreported: bool,
}

/// An earlier registry, one flaw at a time.
#[derive(Clone, Copy, Debug, Default, Hash, PartialEq, Eq)]
struct Broken {
    untie_forgets: bool,
    end_extracts: bool,
    drop_is_evidence: bool,
    pending_ends: bool,
    forgotten_ends: bool,
    unchecked_ends: bool,
    busy_ends: bool,
}

#[derive(Clone, Copy, Debug, Default)]
struct BoundWork {
    broken: Broken,
}

const MAX_BOUNDS: u8 = 2;
const MAX_TAKEN: u8 = 3;
const MAX_CHANGES: u8 = 2;
const CLEAN_MEANS_STOPPED: &str =
    "a key change that reports nothing leaves its work acting nowhere and unable to start";

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

    /// What `seen` counted as: itself, or in round 2 an end when its flaw is on.
    fn counted(self, seen: Seen) -> Seen {
        let ends = match seen {
            Seen::Pending => self.broken.pending_ends,
            Seen::Forgotten => self.broken.forgotten_ends,
            Seen::Unchecked => self.broken.unchecked_ends,
            Seen::Busy => self.broken.busy_ends,
            Seen::ExitCode
            | Seen::Reaped
            | Seen::Joined
            | Seen::ContainerGone
            | Seen::NeverSent
            | Seen::Answered
            | Seen::Landed => false,
        };
        if ends { Seen::ExitCode } else { seen }
    }

    /// A Bound goes, its holder having last seen `seen`, leaving `doubt` without evidence.
    fn release(self, state: &mut State, seen: Seen, doubt: Doubt) {
        state.bounds -= 1;
        if let Some(entry) = &mut state.entry {
            let seen = self.counted(seen);
            if !seen.ended() && !entry.is_ended() {
                state.doubted_live = true;
            }
            entry.release(seen, doubt);
            if state.bounds == 0 && !state.tie_held {
                entry.untie();
            }
        }
        self.settle(state);
    }
}

impl Model for BoundWork {
    type State = State;
    type Action = Action;

    fn init_states(&self) -> Vec<State> {
        vec![State {
            entry: Some(Entry::bound()),
            extracted: false,
            phase: Phase::Pending,
            bounds: 1,
            taken: 1,
            tie_held: true,
            changes: 0,
            changing: false,
            clean: false,
            reported: false,
            doubted_live: false,
            misreported: false,
        }]
    }

    fn actions(&self, state: &State, actions: &mut Vec<Action>) {
        if (state.tie_held || state.bounds > 0)
            && state.bounds < MAX_BOUNDS
            && state.taken < MAX_TAKEN
        {
            actions.push(Action::Hold);
        }
        match state.phase {
            Phase::Pending => actions.extend([Action::Start, Action::Stop]),
            Phase::Acting => actions.push(Action::Stop),
            Phase::Done => {}
        }
        if state.bounds > 0 {
            actions.extend(
                Seen::ALL
                    .into_iter()
                    .filter(|&seen| possible(seen, state.phase))
                    .map(Action::Stopped),
            );
            actions.extend([
                Action::Unconfirmed { recheck: true },
                Action::Unconfirmed { recheck: false },
                Action::Dropped,
            ]);
        }
        if state.tie_held {
            actions.push(Action::Untie);
        }
        if !state.changing && state.changes < MAX_CHANGES {
            actions.push(Action::Change);
        }
        if state.changing {
            let rechecks = state
                .entry
                .as_ref()
                .is_some_and(|entry| entry.doubts().iter().any(|doubt| doubt.recheck));
            let looks = Seen::ALL
                .into_iter()
                .filter(|&look| rechecks && possible(look, state.phase))
                .chain((!rechecks).then_some(Seen::Unchecked));
            for look in looks {
                actions.push(Action::Finish {
                    timed_out: true,
                    look,
                });
                if state.bounds == 0 {
                    actions.push(Action::Finish {
                        timed_out: false,
                        look,
                    });
                }
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
                    state.phase = Phase::Acting;
                }
            }
            Action::Start => state.phase = Phase::Acting,
            Action::Stop => state.phase = Phase::Done,
            Action::Stopped(seen) => {
                if state.phase != Phase::Done {
                    state.misreported = true;
                }
                self.release(&mut state, seen, Doubt { recheck: true });
            }
            Action::Unconfirmed { recheck } => {
                self.release(&mut state, Seen::Unchecked, Doubt { recheck });
            }
            Action::Dropped => {
                let seen = if self.broken.drop_is_evidence {
                    Seen::ExitCode
                } else {
                    Seen::Unchecked
                };
                self.release(&mut state, seen, Doubt { recheck: false });
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
            Action::Finish { timed_out, look } => {
                state.changing = false;
                let reported = state.entry.as_mut().is_some_and(|entry| {
                    if self.broken.end_extracts {
                        // Round 1 looked at nothing again: what the Bounds said, and the
                        // deadline.
                        !entry.doubts().is_empty() || timed_out && entry.holders() > 0
                    } else {
                        entry.confirm(self.counted(look), |doubt| doubt.recheck);
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
                !(state.clean && state.phase != Phase::Done)
            }),
            Property::sometimes(
                "a key change waits for the work to stop and reports nothing",
                |_, state: &State| state.clean && !state.reported,
            ),
            Property::sometimes(
                "a key change reports work that still acts",
                |_, state: &State| state.reported && state.phase == Phase::Acting,
            ),
            Property::sometimes(
                "a later key change clears what an earlier one reported",
                |_, state: &State| state.reported && state.clean && state.entry.is_none(),
            ),
        ];
        if self.broken == Broken::default() {
            properties.extend([
                Property::sometimes(
                    "a key change reports a doubt left before it",
                    |_, state: &State| state.doubted_live && state.reported,
                ),
                Property::sometimes(
                    "a key change reports work Docker has yet to start",
                    |_, state: &State| state.reported && state.phase == Phase::Pending,
                ),
                Property::sometimes(
                    "a stop reported on a false sighting clears once a look sees the work end",
                    |_, state: &State| state.misreported && state.reported && state.clean,
                ),
            ]);
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
                "{n:2}. entry={:?} extracted={} phase={:?} bounds={} tie={} changes={} clean={} reported={}{}",
                state.entry,
                state.extracted,
                state.phase,
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

    /// The first path on which an earlier `broken` registry says a key change stopped work
    /// that still acts or may still start.
    fn earlier(broken: Broken) -> Vec<(State, Option<Action>)> {
        let path = breaks(BoundWork { broken }, CLEAN_MEANS_STOPPED);
        let (last, _) = path.last().expect("a path").clone();
        assert!(last.clean && last.phase != Phase::Done);
        path
    }

    /// Whether some step of `path` took `action`.
    fn took(path: &[(State, Option<Action>)], action: Action) -> bool {
        path.iter().any(|(_, taken)| *taken == Some(action))
    }

    /// Whether a holder or a recheck on `path` saw `seen`.
    fn saw(path: &[(State, Option<Action>)], seen: Seen) -> bool {
        path.iter().any(|(_, action)| match action {
            Some(Action::Stopped(saw) | Action::Finish { look: saw, .. }) => *saw == seen,
            _ => false,
        })
    }

    /// The registry keeps the entry until evidence settles it, reports it to every key
    /// change while unconfirmed, and clears it only once a look sees the work end.
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
        let path = earlier(Broken {
            untie_forgets: true,
            ..Broken::default()
        });
        let (last, _) = path.last().expect("a path");
        assert!(
            last.doubted_live,
            "a Bound left a doubt before the key change"
        );
    }

    /// A key change times out on work still acting and reports it; round 1 had taken the
    /// entry out of the registry, so the next key change found nothing and reported clean.
    #[test]
    fn a_key_change_must_leave_unconfirmed_work_for_the_next() {
        let path = earlier(Broken {
            end_extracts: true,
            ..Broken::default()
        });
        let (last, _) = path.last().expect("a path");
        assert!(last.reported, "an earlier key change reported the work");
    }

    /// A Bound whose task was aborted before it stopped its process counted as stopped in
    /// round 1, so the key change waited for nothing and reported clean.
    #[test]
    fn a_dropped_bound_is_no_evidence_its_work_ended() {
        let path = earlier(Broken {
            drop_is_evidence: true,
            ..Broken::default()
        });
        assert!(took(&path, Action::Dropped));
    }

    /// Docker answers an exec's attach before it marks the exec running, so a look then sees
    /// it not running with no exit code. Round 2 took that for an exit and forgot the exec,
    /// and Docker started the command after the key change reported clean.
    #[test]
    fn an_exec_not_running_without_an_exit_code_may_not_have_started() {
        let path = earlier(Broken {
            pending_ends: true,
            ..Broken::default()
        });
        assert!(saw(&path, Seen::Pending));
    }

    /// dockerd restarts with live-restore while a detached exec runs, and answers 404 for
    /// it. Round 2 took that for an exit, so the command ran on unwatched.
    #[test]
    fn an_exec_docker_forgot_may_still_run() {
        let path = earlier(Broken {
            forgotten_ends: true,
            ..Broken::default()
        });
        assert!(saw(&path, Seen::Forgotten));
    }

    /// The stop of an exec can't read its process's cgroup. Round 2 took the failed read for
    /// a process gone.
    #[test]
    fn a_look_that_failed_is_no_evidence() {
        let path = earlier(Broken {
            unchecked_ends: true,
            ..Broken::default()
        });
        assert!(saw(&path, Seen::Unchecked));
    }

    /// A Build ended Unknown, and its recheck finds the build slot held by an admission still
    /// recovering it. Round 2 took the busy slot for a recovered one.
    #[test]
    fn a_busy_build_slot_is_no_evidence_its_build_ended() {
        let path = earlier(Broken {
            busy_ends: true,
            ..Broken::default()
        });
        assert!(saw(&path, Seen::Busy));
    }
}
