//! A Server S removed from a Cluster of three Members, and the one-way record each Member
//! keeps of it.
//!
//! Each Member holds its own Corrosion replica of S's `machines` row and its own record
//! (`machine.json`): whether it pins S (`trusted_machines`) and whether it holds S removed
//! (`removed_members`). The record survives a crash; an observation in flight does not.
//! M1 admits joins, M2 is any other Member, and M3 joined before S's row reached it, so
//! it trusts S from its join while its replica holds no row.
//!
//! The actors, each step one `RecordOwner::mutate` or one store read of the code it copies:
//!
//! - Forget (`LocalMachine::forget`, local_machine/departure.rs): the denial, `remove_members`, then
//!   `forget_machine`'s delete, which may fail and leave the row. Forget may run again.
//! - An observation (`ReplicatedStore::machines`, corrosion/store.rs): the row read, then
//!   `record_removed`, which records S only on `removed_machines`' deletion evidence, then
//!   the accept filter (`accepts_machine`) and the pin commit (`pin_machines`), which runs
//!   the filter again inside the record write.
//! - A Register resent to M1, as Cloud or a rerun of `ployz server add` sends it after a
//!   Forget, in two steps (`publish_assignment`): `refuse_removed` (join_approval.rs)
//!   checks the local denial, then the deletion evidence; then `register` writes the row,
//!   with gossip free to land in between. The write (`registration_upsert`,
//!   corrosion/store.rs) inserts no row over one its replica deleted, and a write that
//!   left no row refuses and records S removed. A Forget at M1 can't run between the two:
//!   both take M1's exclusive mutation admission.
//! - Gossip between replicas, and a crash of any Member.
//!
//! A row carries cr-sqlite's causal length: odd while it exists, even once deleted. A
//! merge keeps the longer one, so a Register that upserts a row it still holds never
//! outlives a delete; only an insert over a deleted row would, which is why the write
//! refuses one.
//!
//! The properties: a Member that recorded S removed keeps the record, and never pins or
//! accepts S again; no Member records S removed unless someone forgot S; a Member that
//! read the deletion evidence records it before its observation applies; and a replica
//! that held S's row deleted never holds it again.
//!
//! Out of scope: a Server other than S, the WireGuard peers an observation applies (the
//! filter's answer stands for them), Leave, and a Register at a Member other than M1.

use std::fmt::Write as _;

use stateright::{Model, Property};

use crate::harness::Variant;

const MEMBERS: usize = 3;
const NAMES: [&str; MEMBERS] = ["M1", "M2", "M3"];
/// M1 admits joins.
const ADMITTER: usize = 0;
/// Forgets and resent Registers each run at most this often, which is enough to rerun a
/// Forget whose delete failed and to resend a Register after it.
const ATTEMPTS: u8 = 2;

/// S's `machines` row in one replica.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Row {
    Absent,
    Present(u8),
    Deleted(u8),
}

impl Row {
    fn length(self) -> u8 {
        match self {
            Self::Absent => 0,
            Self::Present(length) | Self::Deleted(length) => length,
        }
    }

    fn merge(self, other: Self) -> Self {
        if other.length() > self.length() {
            other
        } else {
            self
        }
    }

    /// What `forget_machine`'s delete leaves.
    fn deleted(self) -> Self {
        match self {
            Self::Present(length) => Self::Deleted(length + 1),
            Self::Absent | Self::Deleted(_) => self,
        }
    }

    /// `removed_machines`: a key with no row.
    fn deleted_evidence(self) -> bool {
        matches!(self, Self::Deleted(_))
    }
}

/// Where a Member's observation stands.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Observing {
    /// It read `row` and has not run `record_removed`.
    Read { row: Row },
    /// It recorded what the evidence showed, and has not filtered.
    Recorded { row: Row },
    /// The filter accepted S, so the snapshot it returns lists S. The pin commit is next.
    Accepted,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct Member {
    replica: Row,
    /// `trusted_machines` holds S.
    pins: bool,
    /// `removed_members` holds S.
    removed: bool,
    observing: Option<Observing>,
    /// Ghost: this replica held S's row deleted at some point.
    saw_deleted: bool,
    /// Ghost: this Member recorded S removed at some point.
    ever_removed: bool,
    /// Ghost: an observation accepted S while this Member held S removed.
    accepted_removed: bool,
    /// Ghost: an observation filtered past deletion evidence it had not recorded.
    filtered_unrecorded: bool,
    /// Ghost: a Forget ran here.
    forgot: bool,
    /// Ghost: `record_removed` found S unlisted with no deletion evidence, and kept it.
    kept_unlisted: bool,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct State {
    members: [Member; MEMBERS],
    forgets: u8,
    registers: u8,
    /// A resent Register passed its check at M1 and has not written.
    registering: bool,
    crashed: bool,
    /// Ghost: some Forget wrote its denial.
    forgotten: bool,
    /// Ghost: a Forget's delete failed.
    delete_failed: bool,
    /// Ghost: a Register was refused, and on what.
    refused_on_denial: bool,
    refused_on_evidence: bool,
    refused_at_write: bool,
    /// Ghost: an observation that read S's row before a Forget answered here accepted S
    /// after it answered.
    accepted_in_flight: bool,
}

impl State {
    fn member(&self, at: usize) -> Member {
        *self.members.get(at).expect("a Member slot")
    }

    fn member_mut(&mut self, at: usize) -> &mut Member {
        self.members.get_mut(at).expect("a Member slot")
    }
}

fn name(at: usize) -> &'static str {
    NAMES.get(at).expect("a Member slot")
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Action {
    /// Forget at a Member; `deletes` is whether its replicated delete succeeds.
    Forget {
        at: usize,
        deletes: bool,
    },
    Gossip {
        from: usize,
        to: usize,
    },
    Read(usize),
    Record(usize),
    Filter(usize),
    Pin(usize),
    RegisterCheck,
    RegisterWrite,
    Crash(usize),
}

/// One change to the code as it ships, each one a guard this model shows is needed.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Mutation {
    /// `accepts_machine` and `pin_machines` skip `removes_member`.
    PinIgnoresRemoved,
    /// The filter checks `removes_member`, but the pin commit does not check it again
    /// inside the record write: the check runs before the commit, not with it.
    RemovalCheckedOutsidePin,
    /// A resent Register skips the local denial and clears it, as the replicated
    /// tombstone clear did for a forgotten row.
    RegisterClearsDenial,
    /// `record_removed` records each trusted Member missing from the snapshot, without
    /// asking `removed_machines`.
    RemovalFromAbsence,
    /// The Register's write trusts its earlier check and inserts over a row its replica
    /// deleted since, as `registration_upsert` did before it asked `machines__crsql_pks`.
    RegisterWritesOverDeleted,
}

#[derive(Clone, Copy, Debug)]
struct MemberRemoval {
    variant: Variant<Mutation>,
}

impl MemberRemoval {
    fn has(self, mutation: Mutation) -> bool {
        self.variant.proposal() == Some(mutation)
    }

    /// `accepts_machine` for S's row as read, with the record as it stands.
    fn accepts(self, member: &Member, row: Row) -> bool {
        matches!(row, Row::Present(_)) && (self.has(Mutation::PinIgnoresRemoved) || !member.removed)
    }

    fn remove(member: &mut Member) {
        member.removed = true;
        member.pins = false;
        member.ever_removed = true;
    }
}

impl Model for MemberRemoval {
    type State = State;
    type Action = Action;

    fn init_states(&self) -> Vec<State> {
        let member = |replica| Member {
            replica,
            pins: true,
            removed: false,
            observing: None,
            saw_deleted: false,
            ever_removed: false,
            accepted_removed: false,
            filtered_unrecorded: false,
            forgot: false,
            kept_unlisted: false,
        };
        vec![State {
            members: [
                member(Row::Present(1)),
                member(Row::Present(1)),
                member(Row::Absent),
            ],
            forgets: 0,
            registers: 0,
            registering: false,
            crashed: false,
            forgotten: false,
            delete_failed: false,
            refused_on_denial: false,
            refused_on_evidence: false,
            refused_at_write: false,
            accepted_in_flight: false,
        }]
    }

    fn actions(&self, state: &State, actions: &mut Vec<Action>) {
        for at in 0..MEMBERS {
            let admitted = at == ADMITTER && state.registering;
            if state.forgets < ATTEMPTS && !admitted {
                actions.push(Action::Forget { at, deletes: true });
                actions.push(Action::Forget { at, deletes: false });
            }
            for to in 0..MEMBERS {
                if to != at && state.member(at).replica.length() > state.member(to).replica.length()
                {
                    actions.push(Action::Gossip { from: at, to });
                }
            }
            actions.push(match state.member(at).observing {
                None => Action::Read(at),
                Some(Observing::Read { .. }) => Action::Record(at),
                Some(Observing::Recorded { .. }) => Action::Filter(at),
                Some(Observing::Accepted) => Action::Pin(at),
            });
            if !state.crashed && (state.member(at).observing.is_some() || admitted) {
                actions.push(Action::Crash(at));
            }
        }
        if state.registering {
            actions.push(Action::RegisterWrite);
        } else if state.registers < ATTEMPTS {
            actions.push(Action::RegisterCheck);
        }
    }

    fn next_state(&self, last: &State, action: Action) -> Option<State> {
        let mut state = last.clone();
        match action {
            Action::Forget { at, deletes } => {
                state.forgets += 1;
                state.forgotten = true;
                let member = state.member_mut(at);
                Self::remove(member);
                member.forgot = true;
                if deletes {
                    member.replica = member.replica.deleted();
                } else {
                    state.delete_failed = true;
                }
            }
            Action::Gossip { from, to } => {
                let row = state.member(from).replica;
                let member = state.member_mut(to);
                member.replica = member.replica.merge(row);
            }
            Action::Read(at) => {
                let member = state.member_mut(at);
                member.observing = Some(Observing::Read {
                    row: member.replica,
                });
            }
            Action::Record(at) => {
                let member = state.member_mut(at);
                let Some(Observing::Read { row }) = member.observing else {
                    return None;
                };
                let unlisted = member.pins && !matches!(row, Row::Present(_));
                let evidence = if self.has(Mutation::RemovalFromAbsence) {
                    unlisted
                } else {
                    unlisted && member.replica.deleted_evidence()
                };
                if evidence {
                    Self::remove(member);
                } else if unlisted {
                    member.kept_unlisted = true;
                }
                member.observing = Some(Observing::Recorded { row });
            }
            Action::Filter(at) => {
                let member = state.member_mut(at);
                let Some(Observing::Recorded { row }) = member.observing else {
                    return None;
                };
                if row.deleted_evidence() && !member.removed && member.pins {
                    member.filtered_unrecorded = true;
                }
                member.observing = if self.accepts(member, row) {
                    if member.removed {
                        member.accepted_removed = true;
                    }
                    Some(Observing::Accepted)
                } else {
                    None
                };
            }
            Action::Pin(at) => {
                let recheck = !self.has(Mutation::RemovalCheckedOutsidePin);
                if state.member(at).removed {
                    state.accepted_in_flight = true;
                }
                let member = state.member_mut(at);
                if !recheck || self.accepts(member, Row::Present(1)) {
                    member.pins = true;
                }
                member.observing = None;
            }
            Action::RegisterCheck => {
                state.registers += 1;
                let clears = self.has(Mutation::RegisterClearsDenial);
                let member = state.member_mut(ADMITTER);
                if member.removed && !clears {
                    state.refused_on_denial = true;
                } else {
                    if clears {
                        member.removed = false;
                    }
                    let evidence = member.replica.deleted_evidence();
                    if evidence {
                        Self::remove(member);
                    }
                    state.refused_on_evidence |= evidence;
                    state.registering = !evidence;
                }
            }
            Action::RegisterWrite => {
                state.registering = false;
                let over_deleted = self.has(Mutation::RegisterWritesOverDeleted);
                let member = state.member_mut(ADMITTER);
                member.replica = match member.replica {
                    Row::Absent => Row::Present(1),
                    Row::Deleted(length) if over_deleted => Row::Present(length + 1),
                    row @ (Row::Present(_) | Row::Deleted(_)) => row,
                };
                if member.replica.deleted_evidence() {
                    Self::remove(member);
                    state.refused_at_write = true;
                }
            }
            Action::Crash(at) => {
                state.crashed = true;
                state.member_mut(at).observing = None;
                if at == ADMITTER {
                    state.registering = false;
                }
            }
        }
        for member in &mut state.members {
            member.saw_deleted |= member.replica.deleted_evidence();
        }
        Some(state)
    }

    fn properties(&self) -> Vec<Property<Self>> {
        vec![
            Property::always(REMOVED_STAYS_OUT, |_, state: &State| {
                state.members.iter().all(|member| {
                    !member.ever_removed
                        || (member.removed && !member.pins && !member.accepted_removed)
                })
            }),
            Property::always(ONLY_A_FORGET_REMOVES, |_, state: &State| {
                state.forgotten || state.members.iter().all(|member| !member.removed)
            }),
            Property::always(EVIDENCE_RECORDED_FIRST, |_, state: &State| {
                state
                    .members
                    .iter()
                    .all(|member| !member.filtered_unrecorded)
            }),
            Property::always(DELETED_STAYS_DELETED, |_, state: &State| {
                state
                    .members
                    .iter()
                    .all(|member| !member.saw_deleted || !matches!(member.replica, Row::Present(_)))
            }),
            Property::sometimes(
                "after a Forget's delete failed, a Forget run again deletes the row",
                |_, state: &State| {
                    state.delete_failed
                        && state
                            .members
                            .iter()
                            .any(|member| member.replica.deleted_evidence())
                },
            ),
            Property::sometimes(
                "a Register resent to the admitter is refused on the denial its Forget left",
                |_, state: &State| state.refused_on_denial,
            ),
            Property::sometimes(
                "a Register resent to the admitter after a Forget elsewhere is refused on the deletion evidence",
                |_, state: &State| state.refused_on_evidence && !state.member(ADMITTER).forgot,
            ),
            Property::sometimes(
                "a Register that passed its check is refused at its write by a delete gossiped in between",
                |_, state: &State| state.refused_at_write,
            ),
            Property::sometimes(
                "a Member that never ran Forget records S removed from deletion evidence",
                |_, state: &State| {
                    state.members[1..]
                        .iter()
                        .any(|member| member.removed && !member.forgot)
                },
            ),
            Property::sometimes(
                "an observation that filtered before Forget answered reaches its pin commit after",
                |_, state: &State| state.accepted_in_flight,
            ),
            Property::sometimes(
                "a Member whose replica never held S's row observes and keeps trusting S",
                |_, state: &State| state.member(2).kept_unlisted,
            ),
        ]
    }
}

const REMOVED_STAYS_OUT: &str =
    "a Member that recorded S removed keeps it removed and never pins or accepts S again";
const ONLY_A_FORGET_REMOVES: &str = "no Member records S removed unless S was forgotten";
const EVIDENCE_RECORDED_FIRST: &str =
    "a Member that read deletion evidence records it before its observation filters";
const DELETED_STAYS_DELETED: &str = "a replica that held S's row deleted never holds S's row again";

fn story(path: &[(State, Option<Action>)]) -> String {
    let mut story = String::new();
    let mut led_by: Option<Action> = None;
    for (n, (state, action)) in path.iter().enumerate() {
        let what = led_by.map_or_else(|| "start".to_owned(), reads_as);
        led_by = *action;
        let _ = writeln!(story, "{n:2}. {what}");
        for (name, member) in NAMES.iter().zip(&state.members) {
            let _ = writeln!(
                story,
                "    {name}: row={:?} pins={} removed={} observing={:?}",
                member.replica, member.pins, member.removed, member.observing,
            );
        }
    }
    story
}

fn reads_as(action: Action) -> String {
    match action {
        Action::Forget { at, deletes } => format!(
            "{}: Forget writes its denial (remove_members), then forget_machine's delete {}",
            name(at),
            if deletes { "succeeds" } else { "fails" }
        ),
        Action::Gossip { from, to } => {
            format!("gossip: {} merges {}'s row", name(to), name(from))
        }
        Action::Read(at) => format!("{}: machines() reads S's row", name(at)),
        Action::Record(at) => format!(
            "{}: record_removed asks removed_machines and records what it shows",
            name(at)
        ),
        Action::Filter(at) => format!("{}: accepts_machine filters the snapshot", name(at)),
        Action::Pin(at) => format!("{}: pin_machines commits", name(at)),
        Action::RegisterCheck => format!(
            "{}: a resent Register checks the denial and the deletion evidence",
            name(ADMITTER)
        ),
        Action::RegisterWrite => format!("{}: the resent Register writes S's row", name(ADMITTER)),
        Action::Crash(at) => format!("{}: crashes; the record stays", name(at)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::{Narrated, breaks, check, last};
    use stateright::Checker;

    impl Narrated for MemberRemoval {
        fn story(&self, path: &[(State, Option<Action>)]) -> String {
            story(path)
        }
    }

    fn mutated(mutation: Mutation) -> MemberRemoval {
        MemberRemoval {
            variant: Variant::Proposed(mutation),
        }
    }

    /// Every interleaving of Forget, its failed delete and its rerun, gossip, each
    /// Member's observation, a resent Register and a crash keeps every property, and
    /// every `sometimes` is reached.
    #[test]
    fn a_removed_server_stays_out_on_every_member_that_recorded_it() {
        let checker = check(MemberRemoval {
            variant: Variant::Shipped,
        });
        println!("as shipped: {} states", checker.unique_state_count());
        checker.assert_properties();
    }

    /// Without the removed check in the filter and the pin, a Member whose Forget's delete
    /// failed still reads S's row, and pins S again.
    #[test]
    fn the_pin_needs_the_removed_filter() {
        let path = breaks(mutated(Mutation::PinIgnoresRemoved), REMOVED_STAYS_OUT);
        assert!(last(&path).members.iter().any(|member| member.ever_removed));
    }

    /// With the removed check made before the pin commits instead of inside it, an
    /// observation that filtered before a Forget answered pins S after it.
    #[test]
    fn the_pin_commit_checks_removal_itself() {
        let path = breaks(
            mutated(Mutation::RemovalCheckedOutsidePin),
            REMOVED_STAYS_OUT,
        );
        let end = last(&path);
        assert!(
            end.members
                .iter()
                .any(|member| member.removed && member.pins)
        );
    }

    /// A Register that clears the denial, as the tombstone clear did, lets the admitter
    /// list and pin S again after a Forget whose delete failed.
    #[test]
    fn a_register_never_clears_a_denial() {
        breaks(mutated(Mutation::RegisterClearsDenial), REMOVED_STAYS_OUT);
    }

    /// A Register whose write trusts its check brings S's row back over a delete that
    /// gossiped into M1 between the two, and the row outlives the delete everywhere.
    #[test]
    fn a_register_writes_no_row_over_a_delete_that_landed_after_its_check() {
        breaks(
            mutated(Mutation::RegisterWritesOverDeleted),
            DELETED_STAYS_DELETED,
        );
    }

    /// Recording removal from a missing row removes S on M3, which joined before S's row
    /// reached it, though nobody forgot S.
    #[test]
    fn removal_is_never_recorded_from_absence() {
        let path = breaks(mutated(Mutation::RemovalFromAbsence), ONLY_A_FORGET_REMOVES);
        assert!(last(&path).members[2].removed);
    }
}
