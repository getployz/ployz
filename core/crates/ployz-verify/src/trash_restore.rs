//! Restores of a deleted Volume's root from the Trash (`ployz volume trash restore`), each
//! run by an operator against the Server whose Trash holds the entry, while others run.
//!
//! A restore pins its entry under its Server's lock (`pin` in
//! core/crates/ployzd/src/volume_plugin/trash.rs), asks each Server in turn for its copy, and
//! sends the promote (`promote`), which may land at any later moment, after the CLI timed
//! out or gave up. The promote runs under its Server's lock and refuses unless the entry is
//! still there with its own pin, the Server holds no root of the Volume, and no other entry
//! of the Volume there is pinned. A named `trash empty`, or the sweep after 7 days, destroys
//! an entry at any moment.
//!
//! DESIGN.md bet 6 needs one writer: no two Servers hold a root of the Volume. On one Server,
//! the promote's check for a root holds it, whatever lands late, even for a caller that sends
//! the promote without asking anyone (`direct`). Across Servers, a restore whose promote is
//! still in flight is invisible to another restore's reads unless each Server's answer lists
//! the pins it holds, which a proof asks for (`prove_copy` in
//! core/crates/ployzd/src/volume_plugin/inspect.rs, `proven` in
//! core/crates/ployz/src/handlers/volume/trash.rs): every restore pins before it reads, so of
//! two in flight, one then reads the other's pin.
//!
//! With `late_join`, B joins the Cluster at any moment, holding its entry, and its own
//! restore can start only then. A restore asks the members of the runtime frame it fetched,
//! after its pin (`restore` in core/crates/ployz/src/handlers/volume/trash.rs). A frame taken
//! before the pin, at connect, misses a Server that joined between the two.
//!
//! The model is a hand-written copy of that logic; it calls none of it.

use std::fmt::Write as _;

use stateright::{Model, Property};

use crate::harness::Narrated;

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub(crate) enum Server {
    A,
    B,
}

/// The Volume's root entries: two in A's Trash, one in B's.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub(crate) enum Entry {
    E1,
    E2,
    E3,
}

impl Entry {
    const fn server(self) -> Server {
        match self {
            Self::E1 | Self::E3 => Server::A,
            Self::E2 => Server::B,
        }
    }
}

/// Where an entry's data is.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Held {
    Trash {
        pinned: bool,
    },
    Gone,
    /// Promoted: its Server's root of the Volume.
    Root,
}

/// One restore, as the CLI runs it.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Run {
    Idle,
    /// Connected to its Server, which saw B as a member or not.
    Connected {
        b: bool,
    },
    /// Pinned, with the Servers that answered no copy so far.
    Reading {
        a: bool,
        b: bool,
    },
    /// The promote is in flight.
    Sent,
    Done,
}

/// One entry's data and its restore.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub(crate) struct Slot {
    held: Held,
    run: Run,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub(crate) struct State {
    e1: Slot,
    e2: Slot,
    e3: Slot,
    /// Whether B is a member yet.
    joined: bool,
}

impl State {
    const fn at(&self, entry: Entry) -> Slot {
        match entry {
            Entry::E1 => self.e1,
            Entry::E2 => self.e2,
            Entry::E3 => self.e3,
        }
    }

    const fn at_mut(&mut self, entry: Entry) -> &mut Slot {
        match entry {
            Entry::E1 => &mut self.e1,
            Entry::E2 => &mut self.e2,
            Entry::E3 => &mut self.e3,
        }
    }

    const fn held(&self) -> [Held; 3] {
        [self.e1.held, self.e2.held, self.e3.held]
    }
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub(crate) enum Action {
    Join,
    Connect(Entry),
    Pin(Entry),
    Read(Entry, Server),
    Send(Entry),
    Promote(Entry),
    Empty(Entry),
}

impl Action {
    const fn entry(self) -> Option<Entry> {
        Some(match self {
            Self::Join => return None,
            Self::Connect(entry)
            | Self::Pin(entry)
            | Self::Read(entry, _)
            | Self::Send(entry)
            | Self::Promote(entry)
            | Self::Empty(entry) => entry,
        })
    }
}

/// A guard taken out, to show it is what holds the property.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub(crate) enum Broken {
    /// The promote skips its check for a root of the Volume on its Server.
    NoLocalRootCheck,
    /// The answers leave out the pins, as before a proof asked for them.
    AnswersWithoutPins,
    /// The restore asks the members of the frame it fetched at connect, before its pin.
    FrameBeforePin,
}

#[derive(Clone, Debug)]
pub(crate) struct TrashRestore {
    broken: Option<Broken>,
    entries: &'static [Entry],
    /// A caller may send the promote right after the pin, skipping the reads.
    direct: bool,
    /// B joins the Cluster during the run.
    late_join: bool,
}

impl TrashRestore {
    const fn on(entries: &'static [Entry]) -> Self {
        Self {
            broken: None,
            entries,
            direct: false,
            late_join: false,
        }
    }

    fn roots(state: &State, server: Server) -> bool {
        Self::of(server).any(|entry| state.at(entry).held == Held::Root)
    }

    fn of(server: Server) -> impl Iterator<Item = Entry> {
        [Entry::E1, Entry::E2, Entry::E3]
            .into_iter()
            .filter(move |entry| entry.server() == server)
    }

    /// Another entry of the Volume on `server` that a restore pinned.
    fn pinned_beside(state: &State, server: Server, own: Entry) -> bool {
        Self::of(server)
            .filter(|entry| *entry != own)
            .any(|entry| state.at(entry).held == Held::Trash { pinned: true })
    }
}

impl Model for TrashRestore {
    type State = State;
    type Action = Action;

    fn init_states(&self) -> Vec<State> {
        let gone = Slot {
            held: Held::Gone,
            run: Run::Idle,
        };
        let mut state = State {
            e1: gone,
            e2: gone,
            e3: gone,
            joined: !self.late_join,
        };
        for entry in self.entries {
            state.at_mut(*entry).held = Held::Trash { pinned: false };
        }
        vec![state]
    }

    fn actions(&self, state: &State, actions: &mut Vec<Action>) {
        if !state.joined {
            actions.push(Action::Join);
        }
        for &entry in self.entries {
            // B's restore starts once B is a member.
            if entry.server() == Server::B && !state.joined {
                continue;
            }
            match state.at(entry).run {
                Run::Idle => actions.push(Action::Connect(entry)),
                Run::Connected { .. } => actions.push(Action::Pin(entry)),
                Run::Reading { a: true, b: true } => actions.push(Action::Send(entry)),
                Run::Reading { a, b } => {
                    if self.direct {
                        actions.push(Action::Send(entry));
                    }
                    for (server, clean) in [(Server::A, a), (Server::B, b)] {
                        if !clean {
                            actions.push(Action::Read(entry, server));
                        }
                    }
                }
                Run::Sent => actions.push(Action::Promote(entry)),
                Run::Done => {}
            }
            if matches!(state.at(entry).held, Held::Trash { .. }) {
                actions.push(Action::Empty(entry));
            }
        }
    }

    fn next_state(&self, state: &State, action: Action) -> Option<State> {
        let mut next = *state;
        let Some(entry) = action.entry() else {
            next.joined = true;
            return Some(next);
        };
        let slot = next.at_mut(entry);
        match action {
            Action::Join => {}
            Action::Connect(_) => slot.run = Run::Connected { b: state.joined },
            Action::Pin(_) => {
                let Run::Connected { b: at_connect } = slot.run else {
                    return None;
                };
                // A Server outside the frame isn't asked, so it reads as answered.
                let asks_b = if self.broken == Some(Broken::FrameBeforePin) {
                    at_connect
                } else {
                    state.joined
                };
                slot.run = match slot.held {
                    Held::Trash { .. } => {
                        slot.held = Held::Trash { pinned: true };
                        Run::Reading {
                            a: false,
                            b: !asks_b,
                        }
                    }
                    Held::Gone | Held::Root => Run::Done,
                };
            }
            Action::Read(entry, server) => {
                let pinned = self.broken != Some(Broken::AnswersWithoutPins)
                    && Self::pinned_beside(state, server, entry);
                if Self::roots(state, server) || pinned {
                    slot.run = Run::Done;
                } else if let Run::Reading { a, b } = slot.run {
                    slot.run = match server {
                        Server::A => Run::Reading { a: true, b },
                        Server::B => Run::Reading { a, b: true },
                    };
                }
            }
            Action::Send(_) => slot.run = Run::Sent,
            Action::Promote(entry) => {
                let server = entry.server();
                let rooted =
                    self.broken != Some(Broken::NoLocalRootCheck) && Self::roots(state, server);
                if slot.held == (Held::Trash { pinned: true })
                    && !rooted
                    && !Self::pinned_beside(state, server, entry)
                {
                    slot.held = Held::Root;
                }
                slot.run = Run::Done;
            }
            Action::Empty(_) => slot.held = Held::Gone,
        }
        Some(next)
    }

    fn properties(&self) -> Vec<Property<Self>> {
        vec![
            Property::always("one writer", |_, state: &State| {
                state
                    .held()
                    .iter()
                    .filter(|held| **held == Held::Root)
                    .count()
                    <= 1
            }),
            Property::sometimes("restored", |_, state: &State| {
                state.held().contains(&Held::Root)
            }),
        ]
    }
}

impl Narrated for TrashRestore {
    fn story(&self, path: &[(State, Option<Action>)]) -> String {
        let mut story = String::new();
        for (state, action) in path {
            let _ = writeln!(story, "  {:?} joined={}", state.held(), state.joined);
            if let Some(action) = action {
                let _ = writeln!(story, "{action:?}");
            }
        }
        story
    }
}

#[cfg(test)]
mod tests {
    use super::{Broken, Entry, TrashRestore};
    use crate::harness::{breaks, check};
    use stateright::Checker;

    const ONE_SERVER: &[Entry] = &[Entry::E1, Entry::E3];
    const BOTH: &[Entry] = &[Entry::E1, Entry::E2, Entry::E3];

    /// On one Server, the promote's check for a root leaves one writer, whatever lands late and
    /// whoever skips the reads; without it, a promote sent without reads lands beside the other
    /// entry's root.
    #[test]
    fn restores_on_one_server_leave_one_writer() {
        let model = TrashRestore {
            direct: true,
            ..TrashRestore::on(ONE_SERVER)
        };
        check(model.clone()).assert_properties();
        breaks(
            TrashRestore {
                broken: Some(Broken::NoLocalRootCheck),
                ..model
            },
            "one writer",
        );
    }

    /// A Server that joins between a restore's connect and its pin is asked, since the
    /// restore fetches the frame after its pin: of two restores in flight, one still reads
    /// the other's pin. A frame taken at connect leaves the joiner unasked, and both land.
    #[test]
    fn a_server_that_joins_before_the_pin_is_asked() {
        let model = TrashRestore {
            late_join: true,
            ..TrashRestore::on(BOTH)
        };
        check(model.clone()).assert_properties();
        breaks(
            TrashRestore {
                broken: Some(Broken::FrameBeforePin),
                ..model
            },
            "one writer",
        );
    }

    /// Across Servers, each answer lists its pins, so of two restores in flight one reads the
    /// other's pin and leaves one writer. Without the pins in the answers, a restore whose
    /// promote is still in flight is invisible to the other's reads, and both land.
    #[test]
    fn restores_across_servers_leave_one_writer_through_the_pins_in_each_answer() {
        check(TrashRestore::on(BOTH)).assert_properties();
        breaks(
            TrashRestore {
                broken: Some(Broken::AnswersWithoutPins),
                ..TrashRestore::on(BOTH)
            },
            "one writer",
        );
    }
}
