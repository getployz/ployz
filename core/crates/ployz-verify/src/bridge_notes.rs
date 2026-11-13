//! The ployz bridge's member notes across daemon starts: `bridge-stopped-members.json`, the
//! members a start stopped so it could move them (`Stopped`,
//! core/crates/ployzd/src/network/linux/stopped.rs), and `bridge-member-addresses.json`, the
//! address each member held before a detach (`Pins`, core/crates/ployzd/src/network/linux/pins.rs).
//! A bare `:n` is a line of core/crates/ployzd/src/network/linux/docker_network.rs.
//!
//! What hosts each step. One `ployzd` process runs on the Machine. Each daemon start runs
//! `NetworkPlane::start` (core/crates/ployzd/src/daemon.rs:274), whose `ensure_docker_network`
//! (:516-553) loads both notes (:540-541), readdresses the bridge when its IPv6 shape is wrong
//! (:571-578), pins its members (:545) and starts the members written down (:546). The Machine
//! API is built and served only after `NetworkPlane::start` returns
//! (core/crates/ployzd/src/daemon.rs:347-372, :411-417), so the operator's own steps on a
//! Container (`ContainerStep::Start`, `Stop` and `Hold`,
//! core/crates/ployzd/src/machine/local_machine/container.rs:1106-1136) run only between daemon
//! starts. Docker's restart policy acts at any step. A crash ends the process: the in-memory
//! `Stopped`, `Pins` and member list go, and the files and Docker's state stay. Each Docker call
//! is one atomic step, and so is each `atomic_write` (core/crates/ployzd/src/filesystem.rs:13-26).
//!
//! The first finding: a member Docker refused to start stays in `bridge-stopped-members.json`
//! (stopped.rs:64-73). The operator stops it between daemon starts, and nothing touches the note
//! (`ContainerRuntime::stop`, core/crates/ployzd/src/docker/lifecycle.rs:611-670). The next daemon
//! start's `Stopped::start` starts it again (:546, stopped.rs:60-79). A failed rewrite of the
//! note after a start that Docker accepted keeps the member written down the same way
//! (stopped.rs:80-93).
//!
//! The second finding: `Stopped::start` (:546) starts every member written down, whether or not
//! `pin_members` rejoined it. `pin_members` returns before rejoining anything when its listing
//! fails (:752-760), and records a member whose `rejoin_with` failed only in `unjoined`
//! (:818-820), so a member still detached from the bridge is started.
//!
//! The fix variant (`BridgeNotes::fix`) reads each member's `MemberDebt` from the two notes, with
//! their formats unchanged. Its `Stopped::start` pays a Start debt only once Docker shows the
//! member attached. A start it cannot pay, one Docker refuses or one it defers, it drops
//! (`OnRefusal::Drop`) or keeps for `RETRIES` later daemon starts (`OnRefusal::Retry`). It keeps
//! the code's order: Docker's start first, then the note's update. Each `Broken` flag removes one
//! guard, the code's or the fix's.

use std::fmt::Write as _;

use ployz_core::RestartPolicy;
use ployz_decide::bridge::{self, Settle};
use stateright::{Model, Property};

use crate::harness::{self, Variant, everywhere};

/// The ployz bridge's name in Docker's messages (`DOCKER_NETWORK_NAME`,
/// core/crates/ployzd/src/network.rs).
const NETWORK: &str = "ployz";

/// Docker's refusal of a disconnect or connect of a member between restarts.
fn refusal_between_restarts(member: Member) -> String {
    format!("network sandbox for container {member:?} not found")
}

/// Docker's refusal of a disconnect of a member with no endpoint on the bridge.
fn refusal_not_connected(member: Member) -> String {
    format!("container {member:?} is not connected to network {NETWORK}")
}

/// Docker's refusal of a `start_container` the model reads, with its status code.
fn refused_start(member: Member, answer: Started) -> (u16, String) {
    match answer {
        Started::Gone => (404, format!("No such container: {member:?}")),
        Started::Netless => (
            404,
            "failed to set up container networking: network 6d85cc7fe1c5 not found".to_owned(),
        ),
        Started::Refused => (500, "refused".to_owned()),
        Started::Ran { .. } | Started::AlreadyRunning | Started::Deferred => {
            unreachable!("Docker accepted or never saw the start")
        }
    }
}

/// Daemon starts. The first finding needs two: the start whose `Stopped::start` Docker refuses,
/// and the start after the operator's stop.
const STARTS: u8 = 2;
/// Crashes, each ending a daemon start at any step.
const CRASHES: u8 = 1;
/// Starts Docker refuses, with a 500 or the netless 404.
const REFUSED_STARTS: u8 = 1;
/// Container listings that fail, in the readdress (:648) or the pin pass (:752).
const FAILED_LISTINGS: u8 = 1;
/// Pinned connects Docker refuses, whose `connect(None)` fallback it then accepts or refuses.
const REFUSED_CONNECTS: u8 = 1;
/// Moves of Docker's restart policy on a member not stopped through the API.
const FLIPS: u8 = 1;
/// Note writes that fail: `Stopped::note`, `Pins::record`, `Pins::finish` or `Stopped::start`'s
/// update.
const FAILED_WRITES: u8 = 1;
/// Operator starts of a Container at rest (`ContainerStep::Start`).
const OPERATOR_STARTS: u8 = 1;
/// Operator stops, holds or removes (`ContainerStep::Stop`, `Hold`, `Remove`).
const OPERATOR_STOPS: u8 = 1;
/// Under the fix's `Retry` policy, how many later daemon starts start a member again after
/// its start was refused or deferred.
const RETRIES: u8 = 1;

/// A bridge member: a Container on the ployz bridge, or created for it (`BridgeMember`,
/// :100-108). The model's members are Service Containers; `M0` is the one the findings follow.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq, PartialOrd, Ord)]
enum Member {
    M0,
    M1,
}

impl Member {
    const ALL: [Self; 2] = [Self::M0, Self::M1];

    const fn bit(self) -> u8 {
        match self {
            Self::M0 => 1,
            Self::M1 => 2,
        }
    }
}

/// One value per member.
#[derive(Clone, Copy, Debug, Default, Hash, PartialEq, Eq)]
struct Pair<T> {
    m0: T,
    m1: T,
}

impl<T> Pair<T> {
    const fn both(value: T) -> Self
    where
        T: Copy,
    {
        Self {
            m0: value,
            m1: value,
        }
    }

    const fn get(&self, member: Member) -> &T {
        match member {
            Member::M0 => &self.m0,
            Member::M1 => &self.m1,
        }
    }

    const fn get_mut(&mut self, member: Member) -> &mut T {
        match member {
            Member::M0 => &mut self.m0,
            Member::M1 => &mut self.m1,
        }
    }
}

/// A set of members, such as the ids `Stopped` holds.
#[derive(Clone, Copy, Debug, Default, Hash, PartialEq, Eq)]
struct Set(u8);

impl Set {
    const fn has(self, member: Member) -> bool {
        self.0 & member.bit() != 0
    }

    const fn with(self, member: Member) -> Self {
        Self(self.0 | member.bit())
    }

    const fn is_empty(self) -> bool {
        self.0 == 0
    }

    fn members(self) -> impl Iterator<Item = Member> {
        Member::ALL
            .into_iter()
            .filter(move |member| self.has(*member))
    }
}

/// A host address of the Machine Subnet, in the subnet's order. The model's subnet holds one
/// more host than it has members, so every member can take a free one.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq, PartialOrd, Ord)]
enum Addr {
    A,
    B,
    C,
}

const HOSTS: [Addr; 3] = [Addr::A, Addr::B, Addr::C];

/// A set of addresses.
#[derive(Clone, Copy, Debug, Default, Hash, PartialEq, Eq)]
struct Addrs(u8);

impl Addrs {
    const fn bit(address: Addr) -> u8 {
        match address {
            Addr::A => 1,
            Addr::B => 2,
            Addr::C => 4,
        }
    }

    const fn has(self, address: Addr) -> bool {
        self.0 & Self::bit(address) != 0
    }

    fn extend(&mut self, address: Option<Addr>) {
        if let Some(address) = address {
            self.0 |= Self::bit(address);
        }
    }

    fn last(self) -> Option<Addr> {
        HOSTS.into_iter().rev().find(|address| self.has(*address))
    }
}

/// `bridge_address::next` (core/crates/ployzd/src/docker/bridge_address.rs:80-102): the first
/// free host after `after`, wrapping, and without `after` past the highest taken address.
fn next(hosts: &[Addr], taken: Addrs, after: Option<Addr>) -> Option<Addr> {
    let after = after.or_else(|| taken.last());
    let start = after.map_or(0, |after| hosts.partition_point(|host| *host <= after));
    hosts
        .iter()
        .cycle()
        .skip(start)
        .take(hosts.len())
        .find(|address| !taken.has(**address))
        .copied()
}

/// Whether Docker runs a member under `policy` again after it exits.
const fn restarts(policy: RestartPolicy) -> bool {
    matches!(policy, RestartPolicy::Always | RestartPolicy::UnlessStopped)
}

/// Whether a member runs, as Docker holds it.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Run {
    Running,
    /// Between restarts: Docker counts it running but it holds no sandbox, so Docker refuses
    /// to detach or attach it (`ployz_decide::bridge::between_restarts`). Under `Restarts` it reads
    /// `restarting`, and under `No` it is exiting and reads `running`.
    Held,
    Exited,
    /// Removed: Docker answers 404 "No such container".
    Gone,
}

/// The member's ployz endpoint, as Docker shows it.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Endpoint {
    Detached,
    /// Attached with no `ipam_config.ipv4_address` (`pinned` false, :138-140).
    Dynamic(Addr),
    /// Attached with its address pinned.
    Pinned(Addr),
}

impl Endpoint {
    const fn address(self) -> Option<Addr> {
        match self {
            Self::Detached => None,
            Self::Dynamic(address) | Self::Pinned(address) => Some(address),
        }
    }

    const fn attached(self) -> bool {
        !matches!(self, Self::Detached)
    }
}

/// A member as Docker holds it.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct Container {
    run: Run,
    endpoint: Endpoint,
    policy: RestartPolicy,
}

/// The ployz bridge.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Bridge {
    /// It fits but its IPv6 shape is not the one the start wants (:571-578).
    Old,
    /// It fits.
    New,
    /// No bridge stands: `inspect_network` answers 404 (:615).
    None,
}

/// `BridgeMember` as a listing reads it (`bridge_members`, :117-151), after `Pins::restore`.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct Seen {
    ipv4: Option<Addr>,
    attached: bool,
    pinned: bool,
}

impl Seen {
    /// `BridgeMember::needs_pin`.
    const fn needs_pin(self) -> bool {
        bridge::needs_pin(self.attached, self.pinned)
    }
}

/// Why a member is in `bridge-stopped-members.json`, a ghost for the rows and the story.
#[derive(Clone, Copy, Debug, Default, Hash, PartialEq, Eq)]
enum Why {
    /// `stop` wrote it down before stopping it (:880-885).
    #[default]
    Noted,
    /// Docker refused its start with something other than a 404 (stopped.rs:67-73).
    Refused,
    /// Docker answered 404 for its missing network (`ployz_decide::bridge::gone`).
    Netless,
    /// Docker started it, and the note's update failed (stopped.rs:80-93).
    Unupdated,
    /// The fix deferred its start: Docker showed it detached.
    Deferred,
}

/// Which pass a step belongs to.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Phase {
    /// `readdress_docker_network` (:639-688).
    Readdress,
    /// `pin_members` (:751-823).
    Pin,
}

/// One statement of `detach`, `stop` or `rejoin_with`.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Step {
    /// The first disconnect (:841), or the pinned connect (:914-917).
    First,
    /// `stop`'s inspect (:859-867) and `ployz_decide::bridge::settle`.
    Inspect,
    /// `stopped.note` (:881-884).
    Note,
    /// `stop_container` (:887-892), after `settle` answered `settle`.
    Stop(Settle),
    /// The disconnect again (:844), or the pinned connect again (:920-923).
    Again,
    /// `connect(None)` (:925-930); `either`: Docker may refuse it too, after it refused the
    /// pinned connect.
    Fallback { either: bool },
}

/// The statement a daemon start runs next.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum At {
    /// `Stopped::load` and `Pins::load` (:540-541).
    Load,
    /// `inspect_network` (:568).
    Shape,
    /// `bridge_members` (:648), or `bridge_containers` (:752).
    List(Phase),
    /// `pins.record` (:656, :769-777).
    Record(Phase),
    /// `detach` of the member at `i` in the pass's order (:664, :805).
    Detach { phase: Phase, i: u8, step: Step },
    /// `replace_docker_network`'s `remove_network` (:699).
    Remove,
    /// `create_bridge` (:618, :704).
    Create(Phase),
    /// `rejoin_with` of the member at `i` (:685, :818).
    Rejoin { phase: Phase, i: u8, step: Step },
    /// `pins.finish` (:822).
    Finish,
    /// `Stopped::start`'s `start_container` of `member` (stopped.rs:64).
    Start(Member),
    /// `Stopped::start`'s removal or rewrite of the note (stopped.rs:80-93).
    Update,
}

/// The ployzd process.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Process {
    Starting(At),
    /// `ensure_docker_network` returned, and the Machine API may serve the operator.
    Serving,
    /// A crash ended the process.
    Down,
}

/// The members a loop visits, in its order.
#[derive(Clone, Copy, Debug, Default, Hash, PartialEq, Eq)]
struct Order(Option<Member>, Option<Member>);

impl Order {
    const fn nth(self, i: u8) -> Option<Member> {
        match i {
            0 => self.0,
            1 => self.1,
            _ => None,
        }
    }

    fn of(members: impl IntoIterator<Item = Member>) -> Self {
        let mut members = members.into_iter();
        Self(members.next(), members.next())
    }
}

/// What one daemon start holds in memory. A crash drops it.
#[derive(Clone, Copy, Debug, Default, Hash, PartialEq, Eq)]
struct Pass {
    /// `Stopped::ids`.
    stopped: Set,
    /// The ids this start wrote down, a ghost for the rows.
    fresh: Set,
    /// `Pins::addresses`.
    pins: Pair<Option<Addr>>,
    /// The listing, after `Pins::restore`.
    seen: Pair<Option<Seen>>,
    /// The loop's order: every listed member in the readdress, sorted by address (:660), and
    /// the members that need a pin in the pin pass, by id (:778, :149).
    order: Order,
    /// The members this readdress detached (:661-665).
    detached: Set,
    /// The address each member rejoins at (`addresses`, :29-48).
    assigned: Pair<Option<Addr>>,
    /// `recorded` (:769-777).
    recorded: bool,
    /// `unjoined` (:779, :818-820).
    unjoined: Set,
    /// The ids `Stopped::start` keeps (stopped.rs:62-79), and why.
    kept: Set,
    kept_why: Pair<Why>,
    /// Under `Retry`, the starts each kept id has been given again.
    kept_retries: Pair<u8>,
}

/// Each budget the model has spent.
#[derive(Clone, Copy, Debug, Default, Hash, PartialEq, Eq)]
struct Used {
    crashes: u8,
    refused_starts: u8,
    failed_listings: u8,
    refused_connects: u8,
    flips: u8,
    failed_writes: u8,
    operator_starts: u8,
    operator_stops: u8,
}

/// How a note write ended.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Write {
    /// Nothing new to write, so nothing was written.
    Unchanged,
    Written,
    /// The write failed and the old file stayed.
    Failed,
    /// `atomic_write` renamed the file into place and then failed to sync the directory
    /// (core/crates/ployzd/src/filesystem.rs:24-25), so the new file stands and the call
    /// returned Err.
    FailedAfterRename,
}

/// What a disconnect answered.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Disconnect {
    /// The member is detached. `address_noted`: `bridge-member-addresses.json` names the address
    /// it held.
    Detached { address_noted: bool },
    /// 404: the member is gone (:848).
    Gone,
    /// `ployz_decide::bridge::between_restarts`.
    BetweenRestarts,
    /// Docker refused it, and `between_restarts` does not read the refusal as that, so `detach`
    /// returns the error.
    Refused,
}

/// What a connect answered.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Connect {
    Pinned(Addr),
    Dynamic(Addr),
    BetweenRestarts,
    /// Another endpoint holds the pinned address.
    InUse,
    /// Docker refused it with another error, or the member is gone.
    Refused,
}

/// What Docker did with a `start_container`.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Started {
    /// It ran the member, which was at rest. `detached`: the member had no ployz endpoint, and
    /// `attached` is where Docker put it, if anywhere.
    Ran {
        detached: bool,
        attached: Option<Addr>,
    },
    /// The member already ran: bollard reads Docker's 304 as `Ok`.
    AlreadyRunning,
    /// 404 "No such container" (`ployz_decide::bridge::gone`).
    Gone,
    /// 500, kept (stopped.rs:67-73).
    Refused,
    /// The netless 404, kept (`ployz_decide::bridge::gone`).
    Netless,
    /// The fix: Docker shows the member detached, so its start waits.
    Deferred,
}

/// What the operator's step on a member is (`plan_container`,
/// core/crates/ployzd/src/machine/local_machine/container.rs:1420-1441).
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Operator {
    /// `ContainerStep::Start` of a Container at rest; `attached` is where Docker put a detached
    /// one, if anywhere.
    Start { attached: Option<Addr> },
    /// `ContainerStep::Stop`, of a running Container (:1436-1440).
    Stop,
    /// `ContainerStep::Hold`, of a Container at rest (:1441).
    Hold,
    /// A remove: the Container is gone.
    Remove,
}

/// The branch a step took, for the `COVERAGE` rows, the properties and the story.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Branch {
    /// A daemon start began and loaded the notes.
    Loaded {
        stopped: bool,
        pins: bool,
    },
    Shape(Bridge),
    Listed {
        phase: Phase,
        ok: bool,
    },
    Recorded {
        phase: Phase,
        write: Write,
    },
    Disconnected {
        phase: Phase,
        member: Member,
        again: bool,
        answer: Disconnect,
    },
    /// A detach failed: the readdress keeps the bridge, or the pin pass moves on.
    DetachFailed {
        phase: Phase,
        member: Member,
    },
    /// The pin pass skips an attached member: `recorded` is false (:783-785).
    Unrecorded {
        member: Member,
    },
    /// `stop`'s inspect: `None` when the member is gone.
    Inspected {
        member: Member,
        settle: Option<Settle>,
    },
    Noted {
        member: Member,
        write: Write,
    },
    /// `stop_container` stopped the member after `settle` answered `settle`; `noted`:
    /// `bridge-stopped-members.json` names it.
    StoppedIt {
        member: Member,
        settle: Settle,
        noted: bool,
    },
    /// `remove_network` removed the old bridge.
    Removed {
        /// A member's endpoint was still on the old bridge.
        attached: bool,
    },
    Created {
        readdress: bool,
    },
    Connected {
        phase: Phase,
        member: Member,
        step: Step,
        answer: Connect,
    },
    /// `rejoin_with` returned false.
    Unjoined {
        phase: Phase,
        member: Member,
    },
    Finished {
        kept: bool,
        write: Write,
    },
    /// `Stopped::start`'s start of `member`. `why` is why the note named it when this start
    /// loaded it, `None` for an id this start wrote down. `operator_stopped`: the operator
    /// stopped it after Ployz last did.
    Start {
        member: Member,
        answer: Started,
        why: Option<Why>,
        operator_stopped: bool,
        /// The id stays in `Stopped` for the update to write.
        kept: bool,
    },
    /// The old bridge's `remove_network`, with an endpoint still attached: Docker refused it.
    RemoveRefused,
    Updated {
        kept: bool,
        write: Write,
    },
    Crashed,
    Flipped {
        member: Member,
        to: Run,
    },
    Operator {
        member: Member,
        step: Operator,
    },
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct State {
    bridge: Bridge,
    docker: Pair<Container>,
    /// `bridge-stopped-members.json`; empty when it does not exist.
    stopped: Set,
    /// Why each member the note names is there.
    why: Pair<Why>,
    /// Under `Retry`, how many later starts the note has given each member. The note's
    /// `BTreeSet<String>` cannot hold it (stopped.rs:18-21): the policy changes its format.
    retries: Pair<u8>,
    /// `bridge-member-addresses.json`.
    pins: Pair<Option<Addr>>,
    process: Process,
    pass: Pass,
    /// Daemon starts begun.
    starts: u8,
    used: Used,
    /// The operator stopped the member after Ployz last stopped it: the ghost the first
    /// property reads. Ployz's stop clears it, and the operator's stop, hold or remove sets it.
    operator_stopped: Pair<bool>,
    took: Option<Branch>,
}

/// Who steps.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Who {
    /// The daemon: the next statement of a start, or the next start.
    Daemon,
    /// The daemon dies at this step.
    Crash,
    /// Docker's restart policy moves `Member`.
    Docker(Member),
    Operator(Member),
}

/// Which way a step goes, where Docker or the disk decides.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Pick {
    Go,
    /// A listing or a write fails, or Docker refuses a connect or a start.
    Fails,
    /// A write fails after its rename landed.
    AfterRename,
    /// Docker answers a start with the netless 404.
    Netless,
    /// Docker attaches the member on this free address: the fallback connect, or a start of a
    /// detached member.
    Dynamic(Addr),
    /// The operator's stop or hold.
    Stop,
    Remove,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct Action {
    who: Who,
    pick: Pick,
}

/// What the fix does with a start it cannot pay: one Docker refused, or one it deferred
/// because Docker shows the member detached.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum OnRefusal {
    /// Drop the id from the note.
    Drop,
    /// Keep the id for `RETRIES` later daemon starts, then drop it.
    Retry,
}

/// Each flag removes one guard: the code's, or the fix's.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each flag removes one independent guard"
)]
struct Broken {
    /// `stop` stops the member before `Stopped::note`, not after it succeeded (:880-885).
    stop_before_note: bool,
    /// A failed `Pins::record` neither abandons the readdress (:656-659) nor skips an attached
    /// member in the pin pass (:783-785).
    detach_unrecorded: bool,
    /// The readdress replaces the bridge after a failed detach too (:673-679).
    replace_after_failed_detach: bool,
    /// `Pins::restore` gives a detached member its noted address though another member holds
    /// it (pins.rs:54-55).
    restore_held: bool,
    /// `gone` reads the netless 404 as gone, as `is_not_found` did before 74391f71f9
    /// (`ployz_decide::bridge::gone`).
    netless_gone: bool,
    /// The fix starts a member Docker shows detached.
    fix_starts_detached: bool,
    /// The fix keeps a start it cannot pay, as the code keeps a refused one.
    fix_keeps_unpaid: bool,
}

/// What the fix reads a member owes, from the two note files with their formats unchanged
/// (stopped.rs:18-21, pins.rs:17-20).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MemberDebt {
    /// `bridge-member-addresses.json` owes it an address.
    Address,
    /// `bridge-stopped-members.json` owes it a start.
    Start,
    /// Both.
    AddressThenStart,
}

fn debt(state: &State, member: Member) -> Option<MemberDebt> {
    match (state.pins.get(member).is_some(), state.stopped.has(member)) {
        (false, false) => None,
        (true, false) => Some(MemberDebt::Address),
        (false, true) => Some(MemberDebt::Start),
        (true, true) => Some(MemberDebt::AddressThenStart),
    }
}

#[derive(Clone, Copy, Debug)]
struct BridgeNotes {
    /// How many members the bridge has, 1 or 2.
    members: u8,
    /// The fix, with its policy; `Shipped` is the code as it is.
    fix: Variant<OnRefusal>,
    broken: Broken,
}

impl BridgeNotes {
    fn members(&self) -> impl Iterator<Item = Member> + use<> {
        Member::ALL.into_iter().take(usize::from(self.members))
    }

    fn hosts(&self) -> &'static [Addr] {
        HOSTS
            .get(..usize::from(self.members) + 1)
            .expect("one more host than members")
    }

    /// Addresses no member's endpoint holds, which Docker may hand out.
    fn free(&self, state: &State) -> Vec<Addr> {
        let mut held = Addrs::default();
        for member in self.members() {
            held.extend(state.docker.get(member).endpoint.address());
        }
        self.hosts()
            .iter()
            .copied()
            .filter(|address| !held.has(*address))
            .collect()
    }

    /// Whether another member's endpoint holds `address`.
    fn held_by_other(&self, state: &State, member: Member, address: Addr) -> bool {
        self.members().any(|other| {
            other != member && state.docker.get(other).endpoint.address() == Some(address)
        })
    }

    /// `bridge_members` (:117-151) and `Pins::restore` (pins.rs:46-60).
    fn list(&self, state: &mut State) {
        let mut seen = Pair::default();
        for member in self.members() {
            let container = state.docker.get(member);
            if container.run == Run::Gone {
                continue;
            }
            *seen.get_mut(member) = Some(Seen {
                ipv4: container.endpoint.address(),
                attached: container.endpoint.attached(),
                pinned: matches!(container.endpoint, Endpoint::Pinned(_)),
            });
        }
        // Pins::restore: a detached member takes the address written down for it, unless
        // another member holds that address now (pins.rs:49-59).
        let mut held = Addrs::default();
        for member in self.members() {
            held.extend(seen.get(member).and_then(|seen| seen.ipv4));
        }
        for member in self.members() {
            let pinned = *state.pass.pins.get(member);
            if let Some(seen) = seen.get_mut(member)
                && seen.ipv4.is_none()
                && let Some(address) = pinned
                && (self.broken.restore_held || !held.has(address))
            {
                held.extend(Some(address));
                seen.ipv4 = Some(address);
            }
        }
        state.pass.seen = seen;
    }

    /// `Pins::record` (pins.rs:62-78): the new addresses, or `None` when nothing changes.
    fn recorded(&self, state: &State) -> Option<Pair<Option<Addr>>> {
        let mut after = state.pass.pins;
        for member in self.members() {
            if let Some(address) = state.pass.seen.get(member).and_then(|seen| seen.ipv4) {
                *after.get_mut(member) = Some(address);
            }
        }
        (after != state.pass.pins).then_some(after)
    }

    /// `addresses` (:29-48) for `order`, from `taken`.
    fn assign(&self, state: &mut State, order: Order, mut taken: Addrs) {
        let mut last = None;
        let mut assigned = Pair::default();
        for member in [order.0, order.1].into_iter().flatten() {
            let ipv4 = state
                .pass
                .seen
                .get(member)
                .and_then(|seen| seen.ipv4)
                .or_else(|| {
                    let next = next(self.hosts(), taken, last);
                    last = next.or(last);
                    next
                });
            taken.extend(ipv4);
            *assigned.get_mut(member) = ipv4;
        }
        state.pass.assigned = assigned;
    }

    /// The listed members' addresses, after `Pins::restore` (:683, :765).
    fn taken(&self, state: &State) -> Addrs {
        let mut taken = Addrs::default();
        for member in self.members() {
            taken.extend(state.pass.seen.get(member).and_then(|seen| seen.ipv4));
        }
        taken
    }

    /// The readdress's next attached member from `i` on (:663), or the step after the loop.
    fn next_detach(&self, state: &State, i: u8) -> At {
        let order = state.pass.order;
        for i in i..2 {
            if let Some(member) = order.nth(i)
                && state
                    .pass
                    .seen
                    .get(member)
                    .is_some_and(|seen| seen.attached)
            {
                return At::Detach {
                    phase: Phase::Readdress,
                    i,
                    step: Step::First,
                };
            }
        }
        At::Remove
    }

    /// The readdress's rejoin loop over the members it detached (:683-686).
    fn start_rejoins(&self, state: &mut State) -> At {
        let detached = state.pass.detached;
        let order = Order::of(
            [state.pass.order.0, state.pass.order.1]
                .into_iter()
                .flatten()
                .filter(|member| detached.has(*member)),
        );
        state.pass.order = order;
        let taken = self.taken(state);
        self.assign(state, order, taken);
        Self::next_rejoin(state, Phase::Readdress, 0)
    }

    /// The rejoin loop's member at `i`, or the step after it.
    fn next_rejoin(state: &State, phase: Phase, i: u8) -> At {
        if state.pass.order.nth(i).is_some() {
            At::Rejoin {
                phase,
                i,
                step: Step::First,
            }
        } else {
            match phase {
                Phase::Readdress => At::List(Phase::Pin),
                Phase::Pin => At::Finish,
            }
        }
    }

    /// The pin pass's member at `i` (:780-820), or `pins.finish`.
    fn next_pin(state: &State, i: u8) -> At {
        let Some(member) = state.pass.order.nth(i) else {
            return At::Finish;
        };
        let attached = state
            .pass
            .seen
            .get(member)
            .is_some_and(|seen| seen.attached);
        if attached {
            At::Detach {
                phase: Phase::Pin,
                i,
                step: Step::First,
            }
        } else {
            At::Rejoin {
                phase: Phase::Pin,
                i,
                step: Step::First,
            }
        }
    }

    /// `Stopped::start`'s next id after `after` (stopped.rs:62-79), or its update.
    /// The fix's start predicate: it pays a member's Start debt only once Docker shows the
    /// member attached. It does not check that the member sits at the address an
    /// `AddressThenStart` debt owes: no property here needs that check.
    fn defers(&self, state: &State, member: Member) -> bool {
        self.fix.is_proposed()
            && !self.broken.fix_starts_detached
            && matches!(
                debt(state, member),
                Some(MemberDebt::Start | MemberDebt::AddressThenStart)
            )
            && !state.docker.get(member).endpoint.attached()
    }

    fn next_start(state: &State, after: Option<Member>) -> At {
        state
            .pass
            .stopped
            .members()
            .find(|member| after.is_none_or(|after| *member > after))
            .map_or(At::Update, At::Start)
    }

    /// Where the loop goes once `phase`'s member at `i` is done.
    fn after_member(state: &mut State, phase: Phase, i: u8) -> At {
        match phase {
            Phase::Readdress => Self::next_rejoin(state, phase, i + 1),
            Phase::Pin => Self::next_pin(state, i + 1),
        }
    }

    /// A write of a note: `Fails` keeps the old file, `AfterRename` lands the new one and fails.
    fn write(state: &mut State, pick: Pick) -> Write {
        match pick {
            Pick::Fails => {
                state.used.failed_writes += 1;
                Write::Failed
            }
            Pick::AfterRename => {
                state.used.failed_writes += 1;
                Write::FailedAfterRename
            }
            Pick::Go | Pick::Netless | Pick::Dynamic(_) | Pick::Stop | Pick::Remove => {
                Write::Written
            }
        }
    }

    /// One step of `stop` (:858-893) for `member`, from `step`; returns the step after it.
    fn stop(&self, state: &mut State, member: Member, step: Step, pick: Pick) -> Step {
        match step {
            Step::Inspect => {
                let container = *state.docker.get(member);
                let (running, restarting) = match container.run {
                    Run::Gone => {
                        state.took = Some(Branch::Inspected {
                            member,
                            settle: None,
                        });
                        return Step::Again;
                    }
                    Run::Running => (true, false),
                    Run::Held => (true, restarts(container.policy)),
                    Run::Exited => (false, false),
                };
                let answer = bridge::settle(running, restarting, Some(container.policy));
                state.took = Some(Branch::Inspected {
                    member,
                    settle: Some(answer),
                });
                match answer {
                    Settle::Leave => Step::Again,
                    Settle::Stop => Step::Stop(Settle::Stop),
                    Settle::StopAndNote if self.broken.stop_before_note => {
                        Step::Stop(Settle::StopAndNote)
                    }
                    Settle::StopAndNote => Step::Note,
                }
            }
            Step::Note => {
                // Stopped::note (stopped.rs:48-56): insert, then write the whole set.
                let write = if state.pass.stopped.has(member) {
                    Write::Unchanged
                } else {
                    Self::write(state, pick)
                };
                state.took = Some(Branch::Noted { member, write });
                // Under `stop_before_note`, the stop already ran.
                let stop = if self.broken.stop_before_note {
                    Step::Again
                } else {
                    Step::Stop(Settle::StopAndNote)
                };
                match write {
                    Write::Unchanged => stop,
                    Write::Written => {
                        state.pass.stopped = state.pass.stopped.with(member);
                        state.pass.fresh = state.pass.fresh.with(member);
                        state.stopped = state.pass.stopped;
                        *state.why.get_mut(member) = Why::Noted;
                        stop
                    }
                    Write::Failed => Step::Again,
                    Write::FailedAfterRename => {
                        state.stopped = state.pass.stopped.with(member);
                        *state.why.get_mut(member) = Why::Noted;
                        Step::Again
                    }
                }
            }
            Step::Stop(answer) => {
                let container = state.docker.get_mut(member);
                if container.run != Run::Gone {
                    container.run = Run::Exited;
                    *state.operator_stopped.get_mut(member) = false;
                }
                state.took = Some(Branch::StoppedIt {
                    member,
                    settle: answer,
                    noted: state.stopped.has(member),
                });
                if answer == Settle::StopAndNote && self.broken.stop_before_note {
                    Step::Note
                } else {
                    Step::Again
                }
            }
            Step::First | Step::Again | Step::Fallback { .. } => step,
        }
    }

    /// Docker's disconnect of `member` (:832-840).
    fn disconnect(state: &mut State, member: Member) -> Disconnect {
        let container = state.docker.get_mut(member);
        match (container.run, container.endpoint) {
            (Run::Gone, _) => Disconnect::Gone,
            (Run::Held, _) => Self::disconnect_refused(&refusal_between_restarts(member)),
            (_, Endpoint::Detached) => Self::disconnect_refused(&refusal_not_connected(member)),
            (
                Run::Running | Run::Exited,
                Endpoint::Dynamic(address) | Endpoint::Pinned(address),
            ) => {
                container.endpoint = Endpoint::Detached;
                Disconnect::Detached {
                    address_noted: *state.pins.get(member) == Some(address),
                }
            }
        }
    }

    /// What `detach` does with a refusal: stop the member when `between_restarts` reads it so.
    fn disconnect_refused(message: &str) -> Disconnect {
        if bridge::between_restarts(message, NETWORK) {
            Disconnect::BetweenRestarts
        } else {
            Disconnect::Refused
        }
    }

    /// Docker's pinned connect of `member` at `ipv4`, before any refusal it may add.
    fn connect(&self, state: &State, member: Member, ipv4: Addr) -> Connect {
        let container = state.docker.get(member);
        match container.run {
            Run::Gone => Connect::Refused,
            Run::Held if bridge::between_restarts(&refusal_between_restarts(member), NETWORK) => {
                Connect::BetweenRestarts
            }
            Run::Held => Connect::Refused,
            Run::Running | Run::Exited if container.endpoint.attached() => Connect::Refused,
            Run::Running | Run::Exited if self.held_by_other(state, member, ipv4) => Connect::InUse,
            Run::Running | Run::Exited => Connect::Pinned(ipv4),
        }
    }

    /// One statement of the daemon start.
    #[expect(
        clippy::too_many_lines,
        reason = "one arm per statement of ensure_docker_network, kept together to read as the code does"
    )]
    fn daemon(&self, state: &mut State, at: At, pick: Pick) {
        let next = match at {
            At::Load => {
                state.pass = Pass {
                    stopped: state.stopped,
                    pins: state.pins,
                    ..Pass::default()
                };
                state.took = Some(Branch::Loaded {
                    stopped: !state.stopped.is_empty(),
                    pins: self
                        .members()
                        .any(|member| state.pins.get(member).is_some()),
                });
                At::Shape
            }
            At::Shape => {
                state.took = Some(Branch::Shape(state.bridge));
                match state.bridge {
                    Bridge::Old => At::List(Phase::Readdress),
                    Bridge::New => At::List(Phase::Pin),
                    Bridge::None => At::Create(Phase::Pin),
                }
            }
            At::List(phase) => {
                if pick == Pick::Fails {
                    state.used.failed_listings += 1;
                    state.took = Some(Branch::Listed { phase, ok: false });
                    match phase {
                        // :650-653: report, and `Ok(())`, so the shape step returns.
                        Phase::Readdress => At::List(Phase::Pin),
                        // :754-759: return before `finish`.
                        Phase::Pin => Self::next_start(state, None),
                    }
                } else {
                    self.list(state);
                    state.took = Some(Branch::Listed { phase, ok: true });
                    match phase {
                        Phase::Readdress => At::Record(Phase::Readdress),
                        Phase::Pin => {
                            let detaches = self.members().any(|member| {
                                state
                                    .pass
                                    .seen
                                    .get(member)
                                    .is_some_and(|seen| seen.attached && seen.needs_pin())
                            });
                            if detaches {
                                At::Record(Phase::Pin)
                            } else {
                                state.pass.recorded = true;
                                self.start_pins(state)
                            }
                        }
                    }
                }
            }
            At::Record(phase) => {
                let write = match self.recorded(state) {
                    None => Write::Unchanged,
                    Some(after) => {
                        let write = Self::write(state, pick);
                        match write {
                            Write::Written => {
                                state.pass.pins = after;
                                state.pins = after;
                            }
                            // `inspect_err` rolls the memory back (pins.rs:75-77).
                            Write::FailedAfterRename => state.pins = after,
                            Write::Failed | Write::Unchanged => {}
                        }
                        write
                    }
                };
                state.took = Some(Branch::Recorded { phase, write });
                let ok = matches!(write, Write::Unchanged | Write::Written);
                match phase {
                    Phase::Readdress if !ok && !self.broken.detach_unrecorded => {
                        At::List(Phase::Pin)
                    }
                    Phase::Readdress => {
                        // :660: sort by (no address, address), stably over the id order.
                        let mut listed = self
                            .members()
                            .filter_map(|member| {
                                state.pass.seen.get(member).map(|seen| (member, seen))
                            })
                            .collect::<Vec<_>>();
                        listed.sort_by_key(|(_, seen)| (seen.ipv4.is_none(), seen.ipv4));
                        state.pass.order = Order::of(listed.into_iter().map(|(member, _)| member));
                        self.next_detach(state, 0)
                    }
                    Phase::Pin => {
                        state.pass.recorded = ok;
                        self.start_pins(state)
                    }
                }
            }
            At::Detach { phase, i, step } => {
                let member = state.pass.order.nth(i).expect("a member at i");
                match step {
                    Step::First | Step::Again => {
                        if phase == Phase::Pin
                            && step == Step::First
                            && !state.pass.recorded
                            && !self.broken.detach_unrecorded
                        {
                            // :783-785.
                            state.took = Some(Branch::Unrecorded { member });
                            Self::next_pin(state, i + 1)
                        } else if phase == Phase::Pin
                            && step == Step::First
                            && state.docker.get(member).run == Run::Gone
                        {
                            // `inspect_container` fails (:796-803).
                            state.took = Some(Branch::Inspected {
                                member,
                                settle: None,
                            });
                            Self::next_pin(state, i + 1)
                        } else {
                            let answer = Self::disconnect(state, member);
                            state.took = Some(Branch::Disconnected {
                                phase,
                                member,
                                again: step == Step::Again,
                                answer,
                            });
                            match (answer, step) {
                                (Disconnect::BetweenRestarts, Step::First) => At::Detach {
                                    phase,
                                    i,
                                    step: Step::Inspect,
                                },
                                (Disconnect::BetweenRestarts | Disconnect::Refused, _) => {
                                    state.took = Some(Branch::DetachFailed { phase, member });
                                    match phase {
                                        // :667-670: `break`, and with `disconnected` an Err
                                        // the bridge is not replaced (:673-679).
                                        Phase::Readdress
                                            if self.broken.replace_after_failed_detach =>
                                        {
                                            At::Remove
                                        }
                                        Phase::Readdress => self.start_rejoins(state),
                                        // :808-814: `continue`.
                                        Phase::Pin => Self::next_pin(state, i + 1),
                                    }
                                }
                                (Disconnect::Detached { .. }, _) => match phase {
                                    Phase::Readdress => {
                                        state.pass.detached = state.pass.detached.with(member);
                                        self.next_detach(state, i + 1)
                                    }
                                    Phase::Pin => At::Rejoin {
                                        phase,
                                        i,
                                        step: Step::First,
                                    },
                                },
                                (Disconnect::Gone, _) => match phase {
                                    Phase::Readdress => self.next_detach(state, i + 1),
                                    Phase::Pin => Self::next_pin(state, i + 1),
                                },
                            }
                        }
                    }
                    Step::Inspect | Step::Note | Step::Stop(_) => At::Detach {
                        phase,
                        i,
                        step: self.stop(state, member, step, pick),
                    },
                    Step::Fallback { .. } => unreachable!("detach has no fallback"),
                }
            }
            At::Remove => {
                // Only under `replace_after_failed_detach` does an endpoint stay attached here.
                // Whether Docker removes such a bridge is not shown in this repo, so the model
                // takes both answers: `Fails` refuses it, and `Go` removes it and its
                // endpoints.
                if pick == Pick::Fails {
                    state.took = Some(Branch::RemoveRefused);
                    self.start_rejoins(state)
                } else {
                    let mut attached = false;
                    for member in self.members() {
                        let endpoint = &mut state.docker.get_mut(member).endpoint;
                        attached |= endpoint.attached();
                        *endpoint = Endpoint::Detached;
                    }
                    state.bridge = Bridge::None;
                    state.took = Some(Branch::Removed { attached });
                    At::Create(Phase::Readdress)
                }
            }
            At::Create(phase) => {
                state.bridge = Bridge::New;
                state.took = Some(Branch::Created {
                    readdress: phase == Phase::Readdress,
                });
                match phase {
                    Phase::Readdress => self.start_rejoins(state),
                    Phase::Pin => At::List(Phase::Pin),
                }
            }
            At::Rejoin { phase, i, step } => {
                let member = state.pass.order.nth(i).expect("a member at i");
                let ipv4 = assigned(state, member);
                match step {
                    Step::First | Step::Again => {
                        let mut answer = self.connect(state, member, ipv4);
                        if pick == Pick::Fails {
                            state.used.refused_connects += 1;
                            answer = Connect::Refused;
                        }
                        if let Connect::Pinned(address) = answer {
                            state.docker.get_mut(member).endpoint = Endpoint::Pinned(address);
                        }
                        state.took = Some(Branch::Connected {
                            phase,
                            member,
                            step,
                            answer,
                        });
                        match answer {
                            Connect::Pinned(_) | Connect::Dynamic(_) => {
                                Self::after_member(state, phase, i)
                            }
                            Connect::BetweenRestarts if step == Step::First => At::Rejoin {
                                phase,
                                i,
                                step: Step::Inspect,
                            },
                            Connect::BetweenRestarts | Connect::InUse | Connect::Refused => {
                                At::Rejoin {
                                    phase,
                                    i,
                                    step: Step::Fallback {
                                        either: pick == Pick::Fails,
                                    },
                                }
                            }
                        }
                    }
                    Step::Inspect | Step::Note | Step::Stop(_) => At::Rejoin {
                        phase,
                        i,
                        step: self.stop(state, member, step, pick),
                    },
                    Step::Fallback { .. } => {
                        let answer = match pick {
                            Pick::Dynamic(address) => {
                                state.docker.get_mut(member).endpoint = Endpoint::Dynamic(address);
                                Connect::Dynamic(address)
                            }
                            Pick::Go
                            | Pick::Fails
                            | Pick::AfterRename
                            | Pick::Netless
                            | Pick::Stop
                            | Pick::Remove => Connect::Refused,
                        };
                        state.took = Some(Branch::Connected {
                            phase,
                            member,
                            step,
                            answer,
                        });
                        if answer == Connect::Refused {
                            if phase == Phase::Pin {
                                state.pass.unjoined = state.pass.unjoined.with(member);
                            }
                            state.took = Some(Branch::Unjoined { phase, member });
                        }
                        Self::after_member(state, phase, i)
                    }
                }
            }
            At::Finish => {
                // Pins::finish (pins.rs:82-100): keep only `unjoined`, then remove or rewrite.
                let mut kept = Pair::default();
                for member in state.pass.unjoined.members() {
                    *kept.get_mut(member) = *state.pass.pins.get(member);
                }
                let write = if kept == state.pins && kept == Pair::default() {
                    Write::Unchanged
                } else {
                    Self::write(state, pick)
                };
                if matches!(write, Write::Written | Write::FailedAfterRename) {
                    state.pins = kept;
                }
                state.took = Some(Branch::Finished {
                    kept: kept != Pair::default(),
                    write,
                });
                Self::next_start(state, None)
            }
            At::Start(member) => {
                let container = *state.docker.get(member);
                let defers = self.defers(state, member);
                let answer = match (container.run, pick) {
                    (Run::Gone, _) => Started::Gone,
                    // The fix reads the member's endpoint before it starts it.
                    (Run::Exited, _) if defers => Started::Deferred,
                    (Run::Running | Run::Held, _) => Started::AlreadyRunning,
                    (Run::Exited, Pick::Fails) => {
                        state.used.refused_starts += 1;
                        Started::Refused
                    }
                    (Run::Exited, Pick::Netless) => {
                        state.used.refused_starts += 1;
                        Started::Netless
                    }
                    (Run::Exited, Pick::Dynamic(address)) => Started::Ran {
                        detached: true,
                        attached: Some(address),
                    },
                    (Run::Exited, Pick::Go | Pick::AfterRename | Pick::Stop | Pick::Remove) => {
                        Started::Ran {
                            detached: !container.endpoint.attached(),
                            attached: None,
                        }
                    }
                };
                let unpaid = match answer {
                    Started::Ran { attached, .. } => {
                        let container = state.docker.get_mut(member);
                        container.run = Run::Running;
                        if let Some(address) = attached {
                            container.endpoint = Endpoint::Dynamic(address);
                        }
                        None
                    }
                    Started::AlreadyRunning => None,
                    Started::Gone | Started::Refused | Started::Netless => {
                        let (status_code, message) = refused_start(member, answer);
                        let dropped = if self.broken.netless_gone {
                            status_code == 404
                        } else {
                            bridge::gone(status_code, &message)
                        };
                        if dropped {
                            None
                        } else if answer == Started::Netless {
                            Some(Why::Netless)
                        } else {
                            Some(Why::Refused)
                        }
                    }
                    Started::Deferred => Some(Why::Deferred),
                };
                let tries = *state.retries.get(member);
                let kept = unpaid.is_some()
                    && match self.fix {
                        _ if self.broken.fix_keeps_unpaid => true,
                        Variant::Shipped => true,
                        Variant::Proposed(OnRefusal::Drop) => false,
                        Variant::Proposed(OnRefusal::Retry) => tries < RETRIES,
                    };
                if let Some(why) = unpaid
                    && kept
                {
                    state.pass.kept = state.pass.kept.with(member);
                    *state.pass.kept_why.get_mut(member) = why;
                    *state.pass.kept_retries.get_mut(member) =
                        if self.fix == Variant::Proposed(OnRefusal::Retry) {
                            tries + 1
                        } else {
                            0
                        };
                }
                state.took = Some(Branch::Start {
                    member,
                    answer,
                    why: (!state.pass.fresh.has(member)).then(|| *state.why.get(member)),
                    operator_stopped: *state.operator_stopped.get(member),
                    kept,
                });
                Self::next_start(state, Some(member))
            }
            At::Update => {
                let kept = state.pass.kept;
                let write = if kept.is_empty() && state.stopped.is_empty() {
                    Write::Unchanged
                } else {
                    Self::write(state, pick)
                };
                match write {
                    Write::Written | Write::FailedAfterRename | Write::Unchanged => {
                        state.stopped = kept;
                        state.retries = state.pass.kept_retries;
                        for member in kept.members() {
                            *state.why.get_mut(member) = *state.pass.kept_why.get(member);
                        }
                    }
                    Write::Failed => {
                        // The old file stays, with every id it held (stopped.rs:88-93).
                        for member in state.stopped.members() {
                            *state.why.get_mut(member) = if kept.has(member) {
                                *state.pass.kept_why.get(member)
                            } else {
                                Why::Unupdated
                            };
                        }
                    }
                }
                state.took = Some(Branch::Updated {
                    kept: !kept.is_empty(),
                    write,
                });
                state.process = Process::Serving;
                return;
            }
        };
        state.process = Process::Starting(next);
    }

    /// The pin pass's loop over the members that need a pin, in id order (:778-780).
    fn start_pins(&self, state: &mut State) -> At {
        let seen = state.pass.seen;
        let order = Order::of(
            self.members()
                .filter(|member| seen.get(*member).is_some_and(|seen| seen.needs_pin())),
        );
        state.pass.order = order;
        let mut taken = self.taken(state);
        // `bridge_address::taken` (core/crates/ployzd/src/docker/bridge_address.rs:58-78): the
        // addresses every attached endpoint holds.
        for member in self.members() {
            taken.extend(state.docker.get(member).endpoint.address());
        }
        self.assign(state, order, taken);
        Self::next_pin(state, 0)
    }

    /// The ways the daemon can step from `at`.
    fn offers(&self, state: &State, at: At) -> Vec<Pick> {
        let writes = |state: &State| {
            if state.used.failed_writes < FAILED_WRITES {
                vec![Pick::Go, Pick::Fails, Pick::AfterRename]
            } else {
                vec![Pick::Go]
            }
        };
        match at {
            At::Remove
                if self
                    .members()
                    .any(|member| state.docker.get(member).endpoint.attached()) =>
            {
                vec![Pick::Go, Pick::Fails]
            }
            At::Load | At::Shape | At::Remove | At::Create(_) => vec![Pick::Go],
            At::List(_) => {
                if state.used.failed_listings < FAILED_LISTINGS {
                    vec![Pick::Go, Pick::Fails]
                } else {
                    vec![Pick::Go]
                }
            }
            At::Record(_) => {
                if self.recorded(state).is_some() {
                    writes(state)
                } else {
                    vec![Pick::Go]
                }
            }
            At::Detach { i, step, .. } | At::Rejoin { i, step, .. } => {
                let member = state.pass.order.nth(i).expect("a member at i");
                match (at, step) {
                    (_, Step::Note) if !state.pass.stopped.has(member) => writes(state),
                    (At::Rejoin { .. }, Step::First | Step::Again) => {
                        let ipv4 = assigned(state, member);
                        let refusable =
                            matches!(self.connect(state, member, ipv4), Connect::Pinned(_));
                        if refusable && state.used.refused_connects < REFUSED_CONNECTS {
                            vec![Pick::Go, Pick::Fails]
                        } else {
                            vec![Pick::Go]
                        }
                    }
                    (At::Rejoin { .. }, Step::Fallback { either }) => {
                        let container = state.docker.get(member);
                        if matches!(container.run, Run::Gone | Run::Held)
                            || container.endpoint.attached()
                        {
                            vec![Pick::Go]
                        } else {
                            let mut picks = self
                                .free(state)
                                .into_iter()
                                .map(Pick::Dynamic)
                                .collect::<Vec<_>>();
                            if either || picks.is_empty() {
                                picks.push(Pick::Go);
                            }
                            picks
                        }
                    }
                    _ => vec![Pick::Go],
                }
            }
            // No `AfterRename` for `finish` or `Stopped::start`'s update: each returns nothing and
            // only prints on Err (pins.rs:94-99, stopped.rs:88-93), so an Err after its rename
            // leaves the files and the run a success leaves.
            At::Finish => {
                let unchanged = state.pins == Pair::default()
                    && state
                        .pass
                        .unjoined
                        .members()
                        .all(|member| state.pass.pins.get(member).is_none());
                if unchanged || state.used.failed_writes >= FAILED_WRITES {
                    vec![Pick::Go]
                } else {
                    vec![Pick::Go, Pick::Fails]
                }
            }
            At::Start(member) => {
                let container = state.docker.get(member);
                if container.run != Run::Exited || self.defers(state, member) {
                    return vec![Pick::Go];
                }
                let mut picks = vec![Pick::Go];
                if !container.endpoint.attached() {
                    picks.extend(self.free(state).into_iter().map(Pick::Dynamic));
                }
                if state.used.refused_starts < REFUSED_STARTS {
                    picks.extend([Pick::Fails, Pick::Netless]);
                }
                picks
            }
            At::Update => {
                if (state.pass.kept.is_empty() && state.stopped.is_empty())
                    || state.used.failed_writes >= FAILED_WRITES
                {
                    vec![Pick::Go]
                } else {
                    vec![Pick::Go, Pick::Fails]
                }
            }
        }
    }

    /// The operator's step on `member`, between daemon starts.
    fn operator(state: &mut State, member: Member, pick: Pick) {
        let container = state.docker.get_mut(member);
        let step = match pick {
            Pick::Stop if container.run == Run::Exited => {
                // ContainerStep::Hold: `containers.stop` (container.rs:1131), which reads
                // Docker's 304 as success (core/crates/ployzd/src/docker/lifecycle.rs:870-881).
                Operator::Hold
            }
            Pick::Stop => {
                container.run = Run::Exited;
                Operator::Stop
            }
            Pick::Remove => {
                container.run = Run::Gone;
                container.endpoint = Endpoint::Detached;
                Operator::Remove
            }
            Pick::Dynamic(address) => {
                container.run = Run::Running;
                container.endpoint = Endpoint::Dynamic(address);
                Operator::Start {
                    attached: Some(address),
                }
            }
            Pick::Go | Pick::Fails | Pick::AfterRename | Pick::Netless => {
                container.run = Run::Running;
                Operator::Start { attached: None }
            }
        };
        match step {
            Operator::Start { .. } => state.used.operator_starts += 1,
            Operator::Stop | Operator::Hold | Operator::Remove => {
                state.used.operator_stops += 1;
                *state.operator_stopped.get_mut(member) = true;
            }
        }
        state.took = Some(Branch::Operator { member, step });
    }
}

impl Model for BridgeNotes {
    type State = State;
    type Action = Action;

    /// Daemon start 1 is about to load the notes. Each member is a Service Container, pinned on
    /// a bridge whose IPv6 shape the start wants changed, and Docker holds `M0` between
    /// restarts. One start state per restart policy `settle` tells apart: the default
    /// (`RestartPolicy::default()`, core/crates/ployz-core/src/host_config.rs:13-14) and `no`.
    fn init_states(&self) -> Vec<State> {
        [RestartPolicy::default(), RestartPolicy::No]
            .into_iter()
            .map(|policy| {
                let member = |run, address| Container {
                    run,
                    endpoint: Endpoint::Pinned(address),
                    policy,
                };
                State {
                    bridge: Bridge::Old,
                    docker: Pair {
                        m0: member(Run::Held, Addr::A),
                        m1: if self.members > 1 {
                            member(Run::Running, Addr::B)
                        } else {
                            Container {
                                run: Run::Gone,
                                endpoint: Endpoint::Detached,
                                policy,
                            }
                        },
                    },
                    stopped: Set::default(),
                    why: Pair::default(),
                    retries: Pair::default(),
                    pins: Pair::default(),
                    process: Process::Starting(At::Load),
                    pass: Pass::default(),
                    starts: 1,
                    used: Used::default(),
                    operator_stopped: Pair::both(false),
                    took: None,
                }
            })
            .collect()
    }

    /// The daemon, a crash, Docker's restart policy, then the operator. The order decides only
    /// which of the shortest paths a story shows.
    fn actions(&self, state: &State, actions: &mut Vec<Action>) {
        match state.process {
            Process::Starting(at) => {
                actions.extend(self.offers(state, at).into_iter().map(|pick| Action {
                    who: Who::Daemon,
                    pick,
                }));
                if state.used.crashes < CRASHES {
                    actions.push(Action {
                        who: Who::Crash,
                        pick: Pick::Go,
                    });
                }
            }
            Process::Serving | Process::Down => {
                if state.starts < STARTS {
                    actions.push(Action {
                        who: Who::Daemon,
                        pick: Pick::Go,
                    });
                }
            }
        }
        for member in self.members() {
            let container = state.docker.get(member);
            if state.used.flips < FLIPS && matches!(container.run, Run::Running | Run::Held) {
                actions.push(Action {
                    who: Who::Docker(member),
                    pick: Pick::Go,
                });
            }
        }
        if state.process == Process::Serving && state.starts < STARTS {
            for member in self.members() {
                let container = state.docker.get(member);
                if container.run == Run::Gone {
                    continue;
                }
                let who = Who::Operator(member);
                if container.run == Run::Exited && state.used.operator_starts < OPERATOR_STARTS {
                    actions.push(Action {
                        who,
                        pick: Pick::Go,
                    });
                    if !container.endpoint.attached() {
                        actions.extend(self.free(state).into_iter().map(|address| Action {
                            who,
                            pick: Pick::Dynamic(address),
                        }));
                    }
                }
                if state.used.operator_stops < OPERATOR_STOPS {
                    actions.extend([
                        Action {
                            who,
                            pick: Pick::Stop,
                        },
                        Action {
                            who,
                            pick: Pick::Remove,
                        },
                    ]);
                }
            }
        }
    }

    fn next_state(&self, last: &State, action: Action) -> Option<State> {
        let mut state = *last;
        state.took = None;
        match action.who {
            Who::Daemon => match state.process {
                Process::Starting(at) => self.daemon(&mut state, at, action.pick),
                Process::Serving | Process::Down => {
                    state.starts += 1;
                    state.process = Process::Starting(At::Load);
                    self.daemon(&mut state, At::Load, Pick::Go);
                }
            },
            Who::Crash => {
                state.used.crashes += 1;
                state.process = Process::Down;
                state.pass = Pass::default();
                state.took = Some(Branch::Crashed);
            }
            Who::Docker(member) => {
                state.used.flips += 1;
                let container = state.docker.get_mut(member);
                container.run = match (container.run, container.policy) {
                    (Run::Running, _) => Run::Held,
                    (Run::Held, policy) if restarts(policy) => Run::Running,
                    (Run::Held, _) => Run::Exited,
                    (Run::Exited | Run::Gone, _) => container.run,
                };
                state.took = Some(Branch::Flipped {
                    member,
                    to: container.run,
                });
            }
            Who::Operator(member) => Self::operator(&mut state, member, action.pick),
        }
        Some(state)
    }

    fn properties(&self) -> Vec<Property<Self>> {
        let mut properties = vec![
            Property::always(STARTS_ONLY_OWED, |_, state: &State| {
                !matches!(
                    state.took,
                    Some(Branch::Start {
                        answer: Started::Ran { .. },
                        operator_stopped: true,
                        ..
                    })
                )
            }),
            Property::always(STARTS_ONLY_ON_BRIDGE, |_, state: &State| {
                !matches!(
                    state.took,
                    Some(Branch::Start {
                        answer: Started::Ran { detached: true, .. },
                        ..
                    })
                )
            }),
            Property::always(NOTED_BEFORE_STOP, |_, state: &State| {
                !matches!(
                    state.took,
                    Some(Branch::StoppedIt {
                        settle: Settle::StopAndNote,
                        noted: false,
                        ..
                    })
                )
            }),
            Property::always(
                ADDRESS_NOTED_BEFORE_DETACH,
                |_, state: &State| match state.took {
                    Some(Branch::Disconnected {
                        member,
                        answer: Disconnect::Detached { .. },
                        ..
                    }) => state.pins.get(member).is_some(),
                    _ => true,
                },
            ),
        ];
        properties.extend(harness::sometimes(COVERAGE, self));
        properties
    }
}

/// `ployz_decide::bridge::Settle::StopAndNote`'s doc comment, `settle`'s, `Stopped::start`'s
/// (stopped.rs:58-59) and the module doc (stopped.rs:1-3) say whom the note starts: a member
/// Docker would run again, stopped so the start could move it. The property narrows that to
/// what the code can know: the start runs only a member Ployz stopped, and that nobody stopped
/// after Ployz did.
const STARTS_ONLY_OWED: &str =
    "Stopped::start starts only a container Ployz stopped and nobody stopped since";
const STARTS_ONLY_ON_BRIDGE: &str =
    "Stopped::start starts a member only while Docker shows it on the bridge";
/// `Stopped::note`'s doc comment (stopped.rs:47): "Writes `id` down. Stop it only once this
/// succeeds." `stop` (:880-885) and its doc (:856-857) say the same.
const NOTED_BEFORE_STOP: &str =
    "the start stops a member it must start again only once the stopped note names it";
/// `Pins::record`'s doc comment (pins.rs:62-63): "Detach a member only once this succeeds."
/// The readdress's doc (:632-634) says the same.
const ADDRESS_NOTED_BEFORE_DETACH: &str =
    "the start detaches a member only while the address note names an address for it";

type Coverage = harness::Coverage<BridgeNotes>;

fn took(state: &State, branch: Branch) -> bool {
    state.took == Some(branch)
}

/// A member is attached on a dynamic address while `bridge-member-addresses.json` owes it
/// another: the model section's risk 3.
fn owes_another(state: &State, member: Member, address: Addr) -> bool {
    state.pins.get(member).is_some_and(|owed| owed != address)
}

/// `Stopped::start` ran `member`, loaded for `why`.
fn ran_loaded(state: &State, why: Why) -> bool {
    matches!(
        state.took,
        Some(Branch::Start {
            answer: Started::Ran { .. },
            why: Some(w),
            ..
        }) if w == why
    )
}

/// Each branch the bounds reach. `current_code_breaks_both_properties_and_reaches_every_row`
/// expects every row on the code as it is, and the two policy tests every row their policy keeps.
const COVERAGE: &[Coverage] = &[
    // ensure_docker_network_shape (:555-622).
    Coverage {
        name: "the start finds the bridge's IPv6 shape wrong and readdresses it",
        on: everywhere,
        omit: None,
        reached: |_, state| took(state, Branch::Shape(Bridge::Old)),
    },
    Coverage {
        name: "the start finds the bridge fitting",
        on: everywhere,
        omit: None,
        reached: |_, state| took(state, Branch::Shape(Bridge::New)),
    },
    Coverage {
        name: "the start finds no bridge and creates one",
        on: everywhere,
        omit: None,
        reached: |_, state| took(state, Branch::Created { readdress: false }),
    },
    // readdress_docker_network (:639-688).
    Coverage {
        name: "the readdress's listing fails, and it returns Ok",
        on: everywhere,
        omit: None,
        reached: |_, state| {
            took(
                state,
                Branch::Listed {
                    phase: Phase::Readdress,
                    ok: false,
                },
            )
        },
    },
    Coverage {
        name: "the readdress writes the address note before any detach",
        on: everywhere,
        omit: None,
        reached: |_, state| {
            took(
                state,
                Branch::Recorded {
                    phase: Phase::Readdress,
                    write: Write::Written,
                },
            )
        },
    },
    Coverage {
        name: "the readdress's address note write fails, and it detaches nothing",
        on: everywhere,
        omit: None,
        reached: |_, state| {
            took(
                state,
                Branch::Recorded {
                    phase: Phase::Readdress,
                    write: Write::Failed,
                },
            )
        },
    },
    Coverage {
        name: "the readdress's address note lands but returns Err, and it detaches nothing",
        on: everywhere,
        omit: None,
        reached: |_, state| {
            took(
                state,
                Branch::Recorded {
                    phase: Phase::Readdress,
                    write: Write::FailedAfterRename,
                },
            )
        },
    },
    Coverage {
        name: "the readdress detaches an attached member at once",
        on: everywhere,
        omit: None,
        reached: |_, state| {
            matches!(
                state.took,
                Some(Branch::Disconnected {
                    phase: Phase::Readdress,
                    again: false,
                    answer: Disconnect::Detached {
                        address_noted: true
                    },
                    ..
                })
            )
        },
    },
    Coverage {
        name: "Docker refuses a disconnect as between restarts",
        on: everywhere,
        omit: None,
        reached: |_, state| {
            matches!(
                state.took,
                Some(Branch::Disconnected {
                    again: false,
                    answer: Disconnect::BetweenRestarts,
                    ..
                })
            )
        },
    },
    Coverage {
        name: "settle answers StopAndNote",
        on: everywhere,
        omit: None,
        reached: |_, state| {
            matches!(
                state.took,
                Some(Branch::Inspected {
                    settle: Some(Settle::StopAndNote),
                    ..
                })
            )
        },
    },
    Coverage {
        name: "settle answers Stop, and the member under no restart policy is stopped without a note",
        on: everywhere,
        omit: None,
        reached: |_, state| {
            matches!(
                state.took,
                Some(Branch::StoppedIt {
                    settle: Settle::Stop,
                    noted: false,
                    ..
                })
            )
        },
    },
    Coverage {
        name: "settle answers Leave for a member that exited before stop's inspect",
        on: everywhere,
        omit: None,
        reached: |_, state| {
            matches!(
                state.took,
                Some(Branch::Inspected {
                    settle: Some(Settle::Leave),
                    ..
                })
            )
        },
    },
    Coverage {
        name: "Stopped::note writes the member down, and stop_container stops it",
        on: everywhere,
        omit: None,
        reached: |_, state| {
            matches!(
                state.took,
                Some(Branch::StoppedIt {
                    settle: Settle::StopAndNote,
                    noted: true,
                    ..
                })
            )
        },
    },
    Coverage {
        name: "Stopped::note's write fails, and the member is left alone",
        on: everywhere,
        omit: None,
        reached: |_, state| {
            matches!(
                state.took,
                Some(Branch::Noted {
                    write: Write::Failed,
                    ..
                })
            )
        },
    },
    Coverage {
        name: "Stopped::note's write lands but returns Err, and the member is left alone",
        on: everywhere,
        omit: None,
        reached: |_, state| {
            matches!(
                state.took,
                Some(Branch::Noted {
                    write: Write::FailedAfterRename,
                    ..
                })
            )
        },
    },
    Coverage {
        name: "the disconnect after a stop detaches the member",
        on: everywhere,
        omit: None,
        reached: |_, state| {
            matches!(
                state.took,
                Some(Branch::Disconnected {
                    again: true,
                    answer: Disconnect::Detached {
                        address_noted: true
                    },
                    ..
                })
            )
        },
    },
    Coverage {
        name: "a readdress detach fails, and the bridge is not replaced",
        on: everywhere,
        omit: None,
        reached: |_, state| {
            matches!(
                state.took,
                Some(Branch::DetachFailed {
                    phase: Phase::Readdress,
                    ..
                })
            ) && state.process != Process::Starting(At::Remove)
        },
    },
    Coverage {
        name: "the readdress replaces the bridge",
        on: everywhere,
        omit: None,
        reached: |_, state| took(state, Branch::Created { readdress: true }),
    },
    Coverage {
        name: "a readdress rejoin pins the member at its address",
        on: everywhere,
        omit: None,
        reached: |_, state| {
            matches!(
                state.took,
                Some(Branch::Connected {
                    phase: Phase::Readdress,
                    answer: Connect::Pinned(_),
                    ..
                })
            )
        },
    },
    Coverage {
        name: "a readdress rejoin fails, and the member stays detached",
        on: everywhere,
        omit: None,
        reached: |_, state| {
            matches!(
                state.took,
                Some(Branch::Unjoined {
                    phase: Phase::Readdress,
                    ..
                })
            )
        },
    },
    Coverage {
        name: "Docker refuses a connect as between restarts, and stop runs",
        on: everywhere,
        omit: None,
        reached: |_, state| {
            matches!(
                state.took,
                Some(Branch::Connected {
                    answer: Connect::BetweenRestarts,
                    step: Step::First,
                    ..
                })
            )
        },
    },
    // pin_members (:751-823).
    Coverage {
        name: "the pin pass's listing fails, and it returns before finish",
        on: everywhere,
        omit: None,
        reached: |_, state| {
            took(
                state,
                Branch::Listed {
                    phase: Phase::Pin,
                    ok: false,
                },
            )
        },
    },
    Coverage {
        name: "the pin pass records an attached member's address before its detach",
        on: everywhere,
        omit: None,
        reached: |_, state| {
            took(
                state,
                Branch::Recorded {
                    phase: Phase::Pin,
                    write: Write::Written,
                },
            )
        },
    },
    Coverage {
        name: "the pin pass's record fails, and it skips an attached member",
        on: everywhere,
        omit: None,
        reached: |_, state| matches!(state.took, Some(Branch::Unrecorded { .. })),
    },
    Coverage {
        name: "the pin pass detaches a member on a dynamic address",
        on: everywhere,
        omit: None,
        reached: |_, state| {
            matches!(
                state.took,
                Some(Branch::Disconnected {
                    phase: Phase::Pin,
                    answer: Disconnect::Detached {
                        address_noted: true
                    },
                    ..
                })
            )
        },
    },
    Coverage {
        name: "the pin pass rejoins a detached member at its noted address",
        on: everywhere,
        omit: None,
        reached: |_, state| {
            matches!(
                state.took,
                Some(Branch::Connected {
                    phase: Phase::Pin,
                    answer: Connect::Pinned(address),
                    member,
                    ..
                }) if *state.pins.get(member) == Some(address)
            )
        },
    },
    Coverage {
        name: "a pin pass rejoin fails, and the member goes into unjoined",
        on: everywhere,
        omit: None,
        reached: |_, state| {
            matches!(
                state.took,
                Some(Branch::Unjoined {
                    phase: Phase::Pin,
                    ..
                })
            )
        },
    },
    Coverage {
        name: "Pins::finish removes the address note",
        on: everywhere,
        omit: None,
        reached: |_, state| {
            took(
                state,
                Branch::Finished {
                    kept: false,
                    write: Write::Written,
                },
            )
        },
    },
    Coverage {
        name: "Pins::finish keeps an unjoined member's address",
        on: everywhere,
        omit: None,
        reached: |_, state| {
            took(
                state,
                Branch::Finished {
                    kept: true,
                    write: Write::Written,
                },
            )
        },
    },
    Coverage {
        name: "Pins::finish fails, and the old address note stays",
        on: everywhere,
        omit: None,
        reached: |_, state| {
            matches!(
                state.took,
                Some(Branch::Finished {
                    write: Write::Failed,
                    ..
                })
            )
        },
    },
    // Stopped::start (stopped.rs:58-94).
    Coverage {
        name: "Stopped::start starts a member this start stopped",
        on: everywhere,
        omit: None,
        reached: |_, state| {
            matches!(
                state.took,
                Some(Branch::Start {
                    answer: Started::Ran {
                        detached: false,
                        ..
                    },
                    why: None,
                    ..
                })
            )
        },
    },
    Coverage {
        name: "Stopped::start finds a noted member already running",
        on: everywhere,
        omit: None,
        reached: |_, state| {
            matches!(
                state.took,
                Some(Branch::Start {
                    answer: Started::AlreadyRunning,
                    ..
                })
            )
        },
    },
    Coverage {
        name: "Stopped::start drops a member the operator removed",
        on: everywhere,
        omit: None,
        reached: |_, state| {
            matches!(
                state.took,
                Some(Branch::Start {
                    answer: Started::Gone,
                    ..
                })
            )
        },
    },
    Coverage {
        name: "Docker refuses a start, and the member stays written down",
        on: everywhere,
        omit: None,
        reached: |_, state| {
            matches!(
                state.took,
                Some(Branch::Start {
                    answer: Started::Refused,
                    kept: true,
                    ..
                })
            )
        },
    },
    Coverage {
        name: "Docker answers the netless 404, and the member stays written down",
        on: everywhere,
        omit: None,
        reached: |_, state| {
            matches!(
                state.took,
                Some(Branch::Start {
                    answer: Started::Netless,
                    kept: true,
                    ..
                })
            )
        },
    },
    Coverage {
        name: "Stopped::start removes the stopped note",
        on: everywhere,
        omit: None,
        reached: |_, state| {
            took(
                state,
                Branch::Updated {
                    kept: false,
                    write: Write::Written,
                },
            )
        },
    },
    Coverage {
        name: "Stopped::start rewrites the stopped note with the members Docker did not start",
        on: everywhere,
        omit: None,
        reached: |_, state| {
            took(
                state,
                Branch::Updated {
                    kept: true,
                    write: Write::Written,
                },
            )
        },
    },
    Coverage {
        name: "Stopped::start's update fails, and the old stopped note stays",
        on: everywhere,
        omit: None,
        reached: |_, state| {
            matches!(
                state.took,
                Some(Branch::Updated {
                    write: Write::Failed,
                    ..
                })
            )
        },
    },
    // What the note keeps, and what a later daemon start does with it.
    Coverage {
        name: "a member Ployz stopped is started at a later daemon start after a start that died",
        on: everywhere,
        omit: None,
        reached: |_, state| state.used.crashes > 0 && ran_loaded(state, Why::Noted),
    },
    // Route 3: the start ran, and the update that should drop the id failed.
    Coverage {
        name: "Stopped::start starts a member the operator stopped, kept because the note's update failed",
        on: everywhere,
        omit: None,
        reached: |_, state| {
            matches!(
                state.took,
                Some(Branch::Start {
                    answer: Started::Ran { .. },
                    why: Some(Why::Unupdated),
                    operator_stopped: true,
                    ..
                })
            )
        },
    },
    Coverage {
        name: "a member Docker refused is started at a later daemon start",
        on: everywhere,
        omit: None,
        reached: |_, state| ran_loaded(state, Why::Refused),
    },
    Coverage {
        name: "a member Docker refused for a missing network is started at a later daemon start",
        on: everywhere,
        omit: None,
        reached: |_, state| ran_loaded(state, Why::Netless),
    },
    // Risk 3: an owed address that a member no longer holds.
    Coverage {
        name: "a refused pinned connect falls back to a dynamic address while the address note owes another",
        on: everywhere,
        omit: None,
        reached: |_, state| {
            matches!(
                state.took,
                Some(Branch::Connected {
                    step: Step::Fallback { .. },
                    answer: Connect::Dynamic(address),
                    member,
                    ..
                }) if owes_another(state, member, address)
            )
        },
    },
    Coverage {
        name: "Docker attaches a detached member it starts on a dynamic address while the address note owes another",
        on: everywhere,
        omit: None,
        reached: |_, state| {
            matches!(
                state.took,
                Some(Branch::Start {
                    answer: Started::Ran {
                        attached: Some(address),
                        ..
                    },
                    member,
                    ..
                }) if owes_another(state, member, address)
            )
        },
    },
    Coverage {
        name: "Stopped::start starts a member attached on a dynamic address while the address note owes another",
        on: everywhere,
        omit: None,
        reached: |_, state| {
            matches!(
                state.took,
                Some(Branch::Start {
                    answer: Started::Ran { detached: false, .. },
                    member,
                    ..
                }) if matches!(
                    state.docker.get(member).endpoint,
                    Endpoint::Dynamic(address) if owes_another(state, member, address)
                )
            )
        },
    },
    // The actors.
    Coverage {
        name: "a daemon start dies",
        on: everywhere,
        omit: None,
        reached: |_, state| took(state, Branch::Crashed),
    },
    Coverage {
        name: "Docker's restart policy runs a held member again",
        on: everywhere,
        omit: None,
        reached: |_, state| {
            matches!(
                state.took,
                Some(Branch::Flipped {
                    to: Run::Running,
                    ..
                })
            )
        },
    },
    Coverage {
        name: "Docker's restart policy holds a running member between restarts",
        on: everywhere,
        omit: None,
        reached: |_, state| matches!(state.took, Some(Branch::Flipped { to: Run::Held, .. })),
    },
    Coverage {
        name: "a held member under no restart policy exits",
        on: everywhere,
        omit: None,
        reached: |_, state| {
            matches!(
                state.took,
                Some(Branch::Flipped {
                    to: Run::Exited,
                    ..
                })
            )
        },
    },
    Coverage {
        name: "the operator starts a member at rest",
        on: everywhere,
        omit: None,
        reached: |_, state| {
            matches!(
                state.took,
                Some(Branch::Operator {
                    step: Operator::Start { .. },
                    ..
                })
            )
        },
    },
    Coverage {
        name: "the operator stops a running member",
        on: everywhere,
        omit: None,
        reached: |_, state| {
            matches!(
                state.took,
                Some(Branch::Operator {
                    step: Operator::Stop,
                    ..
                })
            )
        },
    },
    Coverage {
        name: "the operator holds a member at rest",
        on: everywhere,
        omit: None,
        reached: |_, state| {
            matches!(
                state.took,
                Some(Branch::Operator {
                    step: Operator::Hold,
                    ..
                })
            )
        },
    },
    Coverage {
        name: "the operator removes a member",
        on: everywhere,
        omit: None,
        reached: |_, state| {
            matches!(
                state.took,
                Some(Branch::Operator {
                    step: Operator::Remove,
                    ..
                })
            )
        },
    },
    // The fix's own branches, rows only for a model that proposes it.
    Coverage {
        name: "the fix defers the start of a member Docker shows detached",
        on: |model| model.fix.is_proposed(),
        omit: None,
        reached: |_, state| {
            matches!(
                state.took,
                Some(Branch::Start {
                    answer: Started::Deferred,
                    ..
                })
            )
        },
    },
    Coverage {
        name: "the fix drops a member whose start it cannot pay",
        on: |model| model.fix.is_proposed(),
        omit: None,
        reached: |_, state| {
            matches!(
                state.took,
                Some(Branch::Start {
                    answer: Started::Refused | Started::Netless | Started::Deferred,
                    kept: false,
                    ..
                })
            )
        },
    },
    Coverage {
        name: "the fix drops a member whose retries are spent",
        on: |model| model.fix.is_proposed(),
        omit: None,
        reached: |_, state| {
            matches!(
                state.took,
                Some(Branch::Start {
                    answer: Started::Refused | Started::Netless | Started::Deferred,
                    why: Some(Why::Refused | Why::Netless | Why::Deferred),
                    kept: false,
                    ..
                })
            )
        },
    },
    Coverage {
        name: "a member deferred for its address is started at a later daemon start",
        on: |model| model.fix.is_proposed(),
        omit: None,
        reached: |_, state| ran_loaded(state, Why::Deferred),
    },
];

/// The address `addresses` gave `member` for its rejoin.
fn assigned(state: &State, member: Member) -> Addr {
    state
        .pass
        .assigned
        .get(member)
        .unwrap_or_else(|| panic!("addresses gives {member:?} a free host"))
}

/// `who` as a story names it.
fn name(who: Who) -> String {
    match who {
        Who::Daemon => "ployzd".to_owned(),
        Who::Crash => "crash".to_owned(),
        Who::Docker(member) => format!("Docker's restart policy, {member:?}"),
        Who::Operator(member) => format!("Operator, {member:?}"),
    }
}

/// `path` in the code's words: each step, then the notes, Docker's members and the process.
fn story(model: &BridgeNotes, path: &[(State, Option<Action>)]) -> String {
    let mut story = String::new();
    // Stateright pairs each state with the action taken from it; the row lists the action
    // that led to the state instead, so the last row shows the broken state.
    let mut led_by: Option<(State, Action)> = None;
    for (n, (state, action)) in path.iter().enumerate() {
        let what = led_by.map_or_else(
            || {
                format!(
                    "start: daemon start 1 loads the notes next; {:?} restarts {}",
                    state.docker.m0.run,
                    if restarts(state.docker.m0.policy) {
                        "under unless-stopped"
                    } else {
                        "under no"
                    }
                )
            },
            |(before, action)| reads_as(model, &before, action, state),
        );
        led_by = action.map(|action| (*state, action));
        let _ = writeln!(story, "{n:2}. {what}\n    {}", show(model, state));
    }
    story
}

/// The state after a step, on one line.
fn show(model: &BridgeNotes, state: &State) -> String {
    let mut line = format!(
        "bridge {:?} | stopped note {} | address note {}",
        state.bridge,
        names(model, state.stopped),
        addresses(model, state.pins)
    );
    for member in model.members() {
        let retries = *state.retries.get(member);
        if retries > 0 {
            let _ = write!(line, " | {member:?} retried {retries}");
        }
    }
    for member in model.members() {
        let container = state.docker.get(member);
        let _ = write!(
            line,
            " | {member:?}: {:?}, {}{}",
            container.run,
            match container.endpoint {
                Endpoint::Detached => "detached".to_owned(),
                Endpoint::Dynamic(address) => format!("dynamic {address:?}"),
                Endpoint::Pinned(address) => format!("pinned {address:?}"),
            },
            if *state.operator_stopped.get(member) {
                ", operator stopped it"
            } else {
                ""
            }
        );
    }
    let _ = write!(
        line,
        " | ployzd: {}",
        match state.process {
            Process::Starting(_) => format!("start {}", state.starts),
            Process::Serving => format!("start {} returned, serving", state.starts),
            Process::Down => "down".to_owned(),
        }
    );
    line
}

fn names(model: &BridgeNotes, set: Set) -> String {
    let names = model
        .members()
        .filter(|member| set.has(*member))
        .map(|member| format!("{member:?}"))
        .collect::<Vec<_>>();
    if names.is_empty() {
        "none".to_owned()
    } else {
        format!("{{{}}}", names.join(", "))
    }
}

fn addresses(model: &BridgeNotes, pins: Pair<Option<Addr>>) -> String {
    let names = model
        .members()
        .filter_map(|member| {
            pins.get(member)
                .map(|address| format!("{member:?}: {address:?}"))
        })
        .collect::<Vec<_>>();
    if names.is_empty() {
        "none".to_owned()
    } else {
        format!("{{{}}}", names.join(", "))
    }
}

fn write_words(write: Write, what: &str) -> String {
    match write {
        Write::Unchanged => format!("{what} has nothing new, so nothing is written"),
        Write::Written => format!("{what} is written"),
        Write::Failed => format!("{what} fails, and the old file stays"),
        Write::FailedAfterRename => {
            format!(
                "{what}'s rename lands, then the directory sync fails, so it returns Err (core/crates/ployzd/src/filesystem.rs:24-25)"
            )
        }
    }
}

/// What one step did, in the code's words.
#[expect(
    clippy::too_many_lines,
    reason = "one arm per branch, each in the code's words"
)]
fn reads_as(model: &BridgeNotes, before: &State, action: Action, after: &State) -> String {
    let who = name(action.who);
    let Some(branch) = after.took else {
        return format!("{who}: {action:?}");
    };
    let phase_name = |phase| match phase {
        Phase::Readdress => "readdress",
        Phase::Pin => "pin pass",
    };
    let what = match branch {
        Branch::Loaded { stopped, pins } => format!(
            "daemon start {}: Stopped::load and Pins::load read {} and {} (:540-541)",
            after.starts,
            if stopped {
                "the stopped note"
            } else {
                "no stopped note"
            },
            if pins {
                "the address note"
            } else {
                "no address note"
            }
        ),
        Branch::Shape(Bridge::Old) => "inspect_network: the bridge fits but its IPv6 shape differs, so readdress_docker_network runs (:568-578)".to_owned(),
        Branch::Shape(Bridge::New) => "inspect_network: the bridge fits (:568-578)".to_owned(),
        Branch::Shape(Bridge::None) => "inspect_network answers 404, so create_bridge runs (:615, :618)".to_owned(),
        Branch::Listed { phase: Phase::Readdress, ok: true } => "readdress: bridge_members lists the members, and Pins::restore gives each detached one its noted address (:648, :655)".to_owned(),
        Branch::Listed { phase: Phase::Readdress, ok: false } => "readdress: the listing fails, so it reports and returns Ok (:648-654)".to_owned(),
        Branch::Listed { phase: Phase::Pin, ok: true } => "pin pass: bridge_containers lists the members, and Pins::restore gives each detached one its noted address (:752, :762-765)".to_owned(),
        Branch::Listed { phase: Phase::Pin, ok: false } => "pin pass: the listing fails, so pin_members returns before rejoining anything or calling finish (:752-760)".to_owned(),
        Branch::Recorded { phase, write } => format!(
            "{}: {}{}",
            phase_name(phase),
            write_words(write, "Pins::record's write"),
            match (phase, write) {
                (_, Write::Unchanged | Write::Written) => format!(
                    " ({})",
                    if phase == Phase::Readdress { ":656" } else { ":769-777" }
                ),
                (Phase::Readdress, Write::Failed | Write::FailedAfterRename) => {
                    "; the memory rolls back (pins.rs:75-77), and the readdress returns Ok (:656-659)".to_owned()
                }
                (Phase::Pin, Write::Failed | Write::FailedAfterRename) => {
                    "; the memory rolls back (pins.rs:75-77), and attached members keep their addresses (:769-777, :783-785)".to_owned()
                }
            }
        ),
        Branch::Disconnected {
            phase,
            member,
            again,
            answer,
        } => format!(
            "{}: {}disconnect {member:?} (:{}): {}",
            phase_name(phase),
            if again { "again, " } else { "" },
            if again { "844" } else { "841" },
            match answer {
                Disconnect::Detached { address_noted } => format!(
                    "detached{}",
                    if address_noted {
                        ""
                    } else {
                        ", though the address note does not name its address"
                    }
                ),
                Disconnect::Gone => "404, so detach returns Ok(false) (:848)".to_owned(),
                Disconnect::BetweenRestarts => "Docker refuses it as between restarts (:842, `ployz_decide::bridge::between_restarts`)".to_owned(),
                Disconnect::Refused => "Docker refuses it, and detach returns the error".to_owned(),
            }
        ),
        Branch::DetachFailed { phase, member } => format!(
            "{}: disconnect {member:?} again (:844): Docker refuses it again, so detach returns Err (:849){}",
            phase_name(phase),
            match phase {
                Phase::Readdress => "; the loop breaks, and the bridge is not replaced (:667-679)",
                Phase::Pin => "; pin_members reports and moves on (:808-814)",
            }
        ),
        Branch::Unrecorded { member } => format!(
            "pin pass: {member:?} is attached and recorded is false, so it is skipped (:782-785)"
        ),
        Branch::Inspected {
            member,
            settle: None,
        } => format!("inspect_container {member:?}: 404, so it is left alone"),
        Branch::Inspected {
            member,
            settle: Some(answer),
        } => format!(
            "stop {member:?}: inspect, and settle answers {answer:?} (:859-877, `ployz_decide::bridge::settle`){}",
            match answer {
                Settle::Leave => "; the refused operation is retried",
                Settle::Stop => "; it is stopped without a note",
                Settle::StopAndNote if model.broken.stop_before_note => {
                    "; under stop_before_note, it is stopped before its note"
                }
                Settle::StopAndNote => "; it is noted before its stop",
            }
        ),
        Branch::Noted { member, write } => format!(
            "stop {member:?}: Stopped::note: {}{}",
            write_words(write, "the stopped note"),
            match write {
                Write::Unchanged | Write::Written => " (:881, stopped.rs:48-56)",
                Write::Failed | Write::FailedAfterRename => {
                    "; note takes the id back out (stopped.rs:49-53), and stop reports and leaves it alone (:881-884)"
                }
            }
        ),
        Branch::StoppedIt {
            member,
            settle,
            noted,
        } => format!(
            "stop {member:?}: stop_container (:887-892){}",
            match (settle, noted) {
                (Settle::StopAndNote, true) => ", which the stopped note names",
                (Settle::StopAndNote, false) => ", which the stopped note does not name",
                (Settle::Stop | Settle::Leave, _) => ", for good",
            }
        ),
        Branch::Removed { attached: true } => "readdress: replace_docker_network removes the old bridge, a member still on it, and in this answer Docker detaches the member (:673-679, :699)".to_owned(),
        Branch::Removed { attached: false } => "readdress: every detach succeeded, so replace_docker_network removes the old bridge (:673-679, :699)".to_owned(),
        Branch::RemoveRefused => "readdress: Docker refuses to remove the old bridge, a member still on it, so replace_docker_network returns Ok(Some) (:699-702), and the rejoins run (:683-686)".to_owned(),
        Branch::Created { readdress } => format!(
            "create_bridge creates the bridge in its new shape ({})",
            if readdress { ":704, :709-728" } else { ":618, :709-728" }
        ),
        Branch::Connected {
            phase,
            member,
            step,
            answer,
        } => {
            let call = match (phase, step) {
                (Phase::Readdress, Step::Fallback { .. }) => "readdress: rejoin_with falls back to connect(None) (:925-930)".to_owned(),
                (Phase::Pin, Step::Fallback { .. }) => "pin pass: rejoin_with falls back to connect(None) (:925-930)".to_owned(),
                (_, Step::Again) => format!("{}: rejoin_with connects {member:?} again, pinned (:920-923)", phase_name(phase)),
                (_, Step::First | Step::Inspect | Step::Note | Step::Stop(_)) => format!(
                    "{}: rejoin_with connects {member:?} pinned at {:?} (:{}, :914-917)",
                    phase_name(phase),
                    assigned(after, member),
                    if phase == Phase::Readdress { "897" } else { "818" }
                ),
            };
            let answered = match answer {
                Connect::Pinned(address) => format!("it rejoins pinned at {address:?}"),
                Connect::Dynamic(address) => format!("Docker attaches it on {address:?}, a dynamic address"),
                Connect::BetweenRestarts => "Docker refuses it as between restarts (:918)".to_owned(),
                Connect::InUse => "Docker refuses it: another endpoint holds that address".to_owned(),
                Connect::Refused => "Docker refuses it".to_owned(),
            };
            format!("{call}: {answered}")
        }
        Branch::Unjoined { phase, member } => format!(
            "{}: connect(None) is refused too, so rejoin_with reports and returns false (:925-938){}",
            phase_name(phase),
            match phase {
                Phase::Readdress => format!(", which rejoin discards (:895-898); {member:?} stays detached"),
                Phase::Pin => format!(", and {member:?} goes into unjoined (:818-820)"),
            }
        ),
        Branch::Finished {
            kept: false,
            write: Write::Unchanged,
        } => "pin pass: Pins::finish keeps nothing, and no address note stands to remove (pins.rs:82-88)".to_owned(),
        Branch::Finished { kept, write } => format!(
            "pin pass: Pins::finish keeps {} (pins.rs:82-100): {}",
            if kept { "only unjoined" } else { "nothing" },
            write_words(write, if kept { "the rewrite" } else { "the removal" })
        ),
        Branch::Start {
            member,
            answer,
            why,
            operator_stopped,
            kept,
        } => {
            let loaded = match why {
                None => "written down by this start".to_owned(),
                Some(Why::Noted) => "loaded from a start that died after noting it".to_owned(),
                Some(Why::Refused) => "loaded, kept after Docker refused its start".to_owned(),
                Some(Why::Netless) => "loaded, kept after Docker answered the netless 404".to_owned(),
                Some(Why::Unupdated) => "loaded, kept because the note's last update failed".to_owned(),
                Some(Why::Deferred) => "loaded, kept after the fix deferred its start".to_owned(),
            };
            let answered = match answer {
                Started::Ran { detached: false, .. } => "Docker runs it".to_owned(),
                Started::Ran {
                    detached: true,
                    attached: None,
                } => "Docker runs it, though it has no ployz endpoint".to_owned(),
                Started::Ran {
                    detached: true,
                    attached: Some(address),
                } => format!("Docker runs it, though it had no ployz endpoint, and attaches it on {address:?}"),
                Started::AlreadyRunning => "it already runs: bollard reads Docker's 304 as Ok".to_owned(),
                Started::Gone => "404 No such container, so it is dropped (stopped.rs:66, `ployz_decide::bridge::gone`)".to_owned(),
                Started::Refused if model.fix.is_shipped() => "Docker refuses it (500), so it is kept and reported (stopped.rs:67-73)".to_owned(),
                Started::Netless if model.broken.netless_gone => "Docker answers 404 for its missing network, and gone reads every 404 as gone, so it is dropped".to_owned(),
                Started::Netless if model.fix.is_shipped() => "Docker answers 404 for its missing network, so it is kept (stopped.rs:67-73, `ployz_decide::bridge::gone`)".to_owned(),
                Started::Refused | Started::Netless | Started::Deferred => format!(
                    "{}, and the fix {}",
                    match answer {
                        Started::Netless => "Docker answers 404 for its missing network (stopped.rs:67-73, `ployz_decide::bridge::gone`)",
                        Started::Deferred => "Docker shows it with no ployz endpoint",
                        Started::Refused | Started::Ran { .. } | Started::AlreadyRunning | Started::Gone => {
                            "Docker refuses it (500) (stopped.rs:67-73)"
                        }
                    },
                    if kept { "keeps it for a later daemon start" } else { "drops it" }
                ),
            };
            format!(
                "Stopped::start {} {member:?} ({loaded}{}) (:546, stopped.rs:64): {answered}",
                if answer == Started::Deferred {
                    "defers its start of"
                } else {
                    "starts"
                },
                if operator_stopped {
                    "; the operator stopped it since"
                } else {
                    ""
                }
            )
        }
        Branch::Updated {
            kept: false,
            write: Write::Unchanged,
        } => "Stopped::start keeps nothing, and no stopped note stands to remove (stopped.rs:80-84); ensure_docker_network returns, and the Machine API serves (core/crates/ployzd/src/daemon.rs:347-372)".to_owned(),
        Branch::Updated { kept, write } => format!(
            "Stopped::start {} the stopped note (stopped.rs:80-93): {}; ensure_docker_network returns, and the Machine API serves (core/crates/ployzd/src/daemon.rs:347-372)",
            if kept { "rewrites" } else { "removes" },
            write_words(write, if kept { "the rewrite" } else { "the removal" })
        ),
        Branch::Crashed => format!(
            "daemon start {} dies here: the in-memory Stopped, Pins and member list go",
            after.starts
        ),
        Branch::Flipped { member, to } => format!(
            "Docker's restart policy moves {member:?} from {:?} to {to:?}",
            before.docker.get(member).run
        ),
        Branch::Operator { member, step } => match step {
            Operator::Start { attached } => format!(
                "ContainerStep::Start starts {member:?} (core/crates/ployzd/src/machine/local_machine/container.rs:1099-1105){}",
                attached.map_or_else(String::new, |address| format!(", and Docker attaches it on {address:?}"))
            ),
            Operator::Stop => format!("ContainerStep::Stop stops {member:?}, which runs: stop_out_of_rotation calls containers.stop (core/crates/ployzd/src/machine/local_machine/container.rs:1106-1121, :499-505)"),
            Operator::Hold => format!("ContainerStep::Hold holds {member:?}, at rest: containers.stop, whose 304 reads as success (core/crates/ployzd/src/machine/local_machine/container.rs:1122-1136; core/crates/ployzd/src/docker/lifecycle.rs:870-881)"),
            Operator::Remove => format!("the operator removes {member:?}"),
        },
    };
    format!("{who}: {what}")
}

#[cfg(test)]
mod tests {
    use stateright::Checker;

    use super::*;
    use crate::harness::{
        self, Narrated, breaks, breaks_where, check, first_break, follow, last, reaches,
    };

    type Path = harness::Path<BridgeNotes>;

    impl Narrated for BridgeNotes {
        fn story(&self, path: &[(State, Option<Action>)]) -> String {
            story(self, path)
        }
    }

    /// The code as it is, with one member.
    fn as_is() -> BridgeNotes {
        BridgeNotes {
            members: 1,
            fix: Variant::Shipped,
            broken: Broken::default(),
        }
    }

    /// The branch each step took, in order.
    fn branches(path: &Path) -> Vec<Branch> {
        path.iter()
            .skip(1)
            .filter_map(|(state, _)| state.took)
            .collect()
    }

    const fn daemon(pick: Pick) -> Action {
        Action {
            who: Who::Daemon,
            pick,
        }
    }

    const fn operator(pick: Pick) -> Action {
        Action {
            who: Who::Operator(Member::M0),
            pick,
        }
    }

    const GO: Action = daemon(Pick::Go);
    const CRASH: Action = Action {
        who: Who::Crash,
        pick: Pick::Go,
    };

    /// Daemon start 1 readdresses: it lists M0, restores nothing and records {M0: A}. Docker
    /// refuses M0's disconnect as between restarts; `stop` settles StopAndNote, notes M0 and
    /// stops it, and the second disconnect detaches it. The old bridge is removed, and the
    /// new one created.
    const TO_THE_REJOIN: [Action; 11] = [GO; 11];

    /// The schedule up to `TO_THE_REJOIN`'s detach, before the bridge is replaced.
    const TO_THE_DETACH: [Action; 9] = [GO; 9];

    /// `TO_THE_REJOIN`, then M0 rejoins at A, and the pin pass has nothing to pin, so `finish`
    /// removes the address note. `Stopped::start`'s start of M0 is next.
    const TO_THE_START: [Action; 14] = [GO; 14];

    /// Daemon start 2, on a fitting bridge with M0 pinned: it loads the notes, finds the
    /// bridge fitting, lists M0, and `finish` has nothing to remove. `Stopped::start`'s start
    /// of M0 is next.
    const START_2: [Action; 4] = [GO; 4];

    fn schedule(parts: &[&[Action]]) -> Vec<Action> {
        parts.iter().flat_map(|part| part.iter().copied()).collect()
    }

    /// The first finding: Docker refuses `Stopped::start`'s start of M0 with a 500, so M0 stays
    /// written down. The operator starts M0, then stops it through `ContainerStep::Stop`.
    /// Start 2 finds the bridge fitting, and `Stopped::start` runs M0.
    fn the_refusal() -> Vec<Action> {
        schedule(&[
            &TO_THE_START,
            &[
                daemon(Pick::Fails),
                GO,
                operator(Pick::Go),
                operator(Pick::Stop),
            ],
            &START_2,
            &[GO],
        ])
    }

    /// The first property's second route: Docker runs M0, and `Stopped::start`'s removal of
    /// the note fails, so the old note keeps M0. The operator stops M0, and start 2 runs it.
    fn the_failed_update() -> Vec<Action> {
        schedule(&[
            &TO_THE_START,
            &[GO, daemon(Pick::Fails), operator(Pick::Stop)],
            &START_2,
            &[GO],
        ])
    }

    /// The second finding's early return: start 1's rejoin of M0 is refused pinned and refused
    /// again by `connect(None)`, so M0 stays detached. The pin pass's listing fails, so
    /// `pin_members` returns before rejoining it, and `Stopped::start` runs M0.
    fn the_early_return() -> Vec<Action> {
        schedule(&[
            &TO_THE_REJOIN,
            &[daemon(Pick::Fails), GO, daemon(Pick::Fails), GO],
        ])
    }

    /// The second finding's false rejoin: start 1 dies after detaching M0. Start 2's readdress
    /// lists M0 detached, detaches nothing and replaces the bridge, so it rejoins nothing. The
    /// pin pass restores A for M0, and Docker refuses the pinned connect and `connect(None)`,
    /// so `rejoin_with` returns false. `finish` keeps {M0: A}, and `Stopped::start` runs M0.
    fn the_false_rejoin() -> Vec<Action> {
        schedule(&[
            &TO_THE_DETACH,
            &[CRASH, GO, GO, GO, GO, GO, GO, GO],
            &[daemon(Pick::Fails), GO, GO, GO],
        ])
    }

    fn ran_after_an_operator_stop(state: &State, why: Why) -> bool {
        matches!(
            state.took,
            Some(Branch::Start {
                answer: Started::Ran { .. },
                operator_stopped: true,
                why: Some(w),
                ..
            }) if w == why
        )
    }

    fn ran_detached(state: &State) -> bool {
        matches!(
            state.took,
            Some(Branch::Start {
                answer: Started::Ran { detached: true, .. },
                ..
            })
        )
    }

    /// The first finding, on the code as it is: a member Docker refused stays written down,
    /// the operator stops it, and the next daemon start runs it. The checker's first path to
    /// a start that breaks the property with a kept refusal ends the same way.
    #[test]
    fn current_code_starts_a_refused_member_the_operator_stopped() {
        let path = follow(as_is(), &the_refusal());
        let taken = branches(&path);
        assert!(taken.contains(&Branch::Inspected {
            member: Member::M0,
            settle: Some(Settle::StopAndNote),
        }));
        assert!(taken.contains(&Branch::Start {
            member: Member::M0,
            answer: Started::Refused,
            why: None,
            operator_stopped: false,
            kept: true,
        }));
        assert!(taken.contains(&Branch::Operator {
            member: Member::M0,
            step: Operator::Start { attached: None },
        }));
        assert!(taken.contains(&Branch::Operator {
            member: Member::M0,
            step: Operator::Stop,
        }));
        let after = last(&path);
        assert!(ran_after_an_operator_stop(&after, Why::Refused));
        assert_eq!(after.docker.m0.run, Run::Running);
        assert_eq!(after.docker.m0.endpoint, Endpoint::Pinned(Addr::A));
        assert_eq!(after.starts, 2);

        let found = breaks_where(as_is(), STARTS_ONLY_OWED, |path| {
            ran_after_an_operator_stop(&last(path), Why::Refused)
        });
        assert!(branches(&found).iter().any(|branch| matches!(
            branch,
            Branch::Operator {
                step: Operator::Stop | Operator::Hold,
                ..
            }
        )));
    }

    /// The first property's second route: no start is refused. Docker runs M0, and only the
    /// note's update fails, so the old note keeps M0 (stopped.rs:80-93).
    #[test]
    fn current_code_starts_a_member_whose_note_update_failed_after_the_operator_stopped_it() {
        let path = follow(as_is(), &the_failed_update());
        let taken = branches(&path);
        assert!(taken.contains(&Branch::Updated {
            kept: false,
            write: Write::Failed,
        }));
        assert!(!taken.iter().any(|branch| matches!(
            branch,
            Branch::Start {
                answer: Started::Refused | Started::Netless,
                ..
            }
        )));
        assert!(ran_after_an_operator_stop(&last(&path), Why::Unupdated));

        let found = breaks_where(as_is(), STARTS_ONLY_OWED, |path| {
            ran_after_an_operator_stop(&last(path), Why::Unupdated)
        });
        assert!(ran_after_an_operator_stop(&last(&found), Why::Unupdated));
    }

    /// The second finding's early return, on the code as it is: `pin_members` returns before
    /// rejoining M0, and `Stopped::start` runs it detached. The checker's first path to a
    /// detached start through a failed pin-pass listing ends the same way.
    #[test]
    fn current_code_starts_a_member_pin_members_returned_before_rejoining() {
        let path = follow(as_is(), &the_early_return());
        let taken = branches(&path);
        assert!(taken.contains(&Branch::Unjoined {
            phase: Phase::Readdress,
            member: Member::M0,
        }));
        assert!(taken.contains(&Branch::Listed {
            phase: Phase::Pin,
            ok: false,
        }));
        let after = last(&path);
        assert!(ran_detached(&after));
        assert_eq!(after.docker.m0.endpoint, Endpoint::Detached);
        assert_eq!(after.pins.m0, Some(Addr::A), "the address is still owed");

        let found = breaks_where(as_is(), STARTS_ONLY_ON_BRIDGE, |path| {
            branches(path).contains(&Branch::Listed {
                phase: Phase::Pin,
                ok: false,
            })
        });
        assert!(ran_detached(&last(&found)));
    }

    /// The second finding's false rejoin, on the code as it is: `rejoin_with` returns false,
    /// M0 goes into `unjoined`, and `Stopped::start` runs it detached.
    #[test]
    fn current_code_starts_a_member_whose_rejoin_failed() {
        let path = follow(as_is(), &the_false_rejoin());
        let taken = branches(&path);
        assert!(taken.contains(&Branch::Crashed));
        assert!(taken.contains(&Branch::Unjoined {
            phase: Phase::Pin,
            member: Member::M0,
        }));
        assert!(!taken.contains(&Branch::Listed {
            phase: Phase::Pin,
            ok: false,
        }));
        let after = last(&path);
        assert!(ran_detached(&after));
        assert_eq!(after.pins.m0, Some(Addr::A), "finish kept the owed address");

        let found = breaks_where(as_is(), STARTS_ONLY_ON_BRIDGE, |path| {
            branches(path).iter().any(|branch| {
                matches!(
                    branch,
                    Branch::Unjoined {
                        phase: Phase::Pin,
                        ..
                    }
                )
            })
        });
        assert!(ran_detached(&last(&found)));
    }

    /// The code as it is breaks both findings' properties, keeps its two guards' properties,
    /// and reaches every row.
    #[test]
    fn current_code_breaks_both_properties_and_reaches_every_row() {
        let checker = check(as_is());
        println!("as is: {} states", checker.unique_state_count());
        for property in [STARTS_ONLY_OWED, STARTS_ONLY_ON_BRIDGE] {
            assert!(checker.discovery(property).is_some(), "{property} holds");
        }
        for property in [NOTED_BEFORE_STOP, ADDRESS_NOTED_BEFORE_DETACH] {
            assert!(checker.discovery(property).is_none(), "{property} fails");
        }
        harness::assert_reached(COVERAGE, &as_is(), &checker);
        breaks(as_is(), STARTS_ONLY_OWED);
        breaks(as_is(), STARTS_ONLY_ON_BRIDGE);
    }

    fn fixed(on_refusal: OnRefusal) -> BridgeNotes {
        BridgeNotes {
            fix: Variant::Proposed(on_refusal),
            ..as_is()
        }
    }

    fn broken(fix: Variant<OnRefusal>, broken: Broken) -> BridgeNotes {
        BridgeNotes {
            members: 1,
            fix,
            broken,
        }
    }

    fn holds_at(model: BridgeNotes, property: &str, state: &State) -> bool {
        let named = model
            .properties()
            .into_iter()
            .find(|candidate| candidate.name == property)
            .expect("a model property");
        (named.condition)(&model, state)
    }

    fn row(name: &str) -> &'static Coverage {
        COVERAGE.iter().find(|row| row.name == name).expect("a row")
    }

    /// The rows of `model` its checker never reaches, in `COVERAGE` order.
    fn unreached(model: BridgeNotes, checker: &impl Checker<BridgeNotes>) -> Vec<&'static str> {
        COVERAGE
            .iter()
            .filter(|row| (row.on)(&model))
            .filter(|row| checker.discovery(row.name).is_none())
            .map(|row| row.name)
            .collect()
    }

    /// A P1 break whose member was loaded for anything but a failed note update.
    fn off_route_3(path: &Path) -> bool {
        !matches!(
            last(path).took,
            Some(Branch::Start {
                why: Some(Why::Unupdated),
                ..
            })
        )
    }

    /// Daemon start 2 at `Stopped::start`'s start of M0: the bridge fits, the stopped note
    /// names M0 from start 1, Docker has M0 at rest on `endpoint`, and the address note owes
    /// it `owed`.
    fn at_start(model: BridgeNotes, endpoint: Endpoint, owed: Option<Addr>) -> State {
        let mut state = model
            .init_states()
            .into_iter()
            .next()
            .expect("a start state");
        state.bridge = Bridge::New;
        state.docker.m0.run = Run::Exited;
        state.docker.m0.endpoint = endpoint;
        state.stopped = Set::default().with(Member::M0);
        state.why.m0 = Why::Noted;
        state.pins.m0 = owed;
        state.starts = 2;
        state.pass = Pass {
            stopped: state.stopped,
            ..Pass::default()
        };
        state.process = Process::Starting(At::Start(Member::M0));
        state
    }

    /// One daemon step from `state`, which must offer `pick`.
    fn step(model: BridgeNotes, state: &State, pick: Pick) -> State {
        let mut offered = Vec::new();
        model.actions(state, &mut offered);
        assert!(offered.contains(&daemon(pick)), "{pick:?} is offered");
        model.next_state(state, daemon(pick)).expect("a next state")
    }

    fn started(state: &State) -> (Started, bool) {
        match state.took {
            Some(Branch::Start { answer, kept, .. }) => (answer, kept),
            other => panic!("a start, not {other:?}"),
        }
    }

    /// The code as it is, at the first property's edge states. Each follows `the_refusal`'s
    /// start 1, so M0 is kept at rest, then runs `operator_steps` and daemon start 2.
    fn refusal_then(operator_steps: &[Action]) -> State {
        last(&follow(
            as_is(),
            &schedule(&[
                &TO_THE_START,
                &[daemon(Pick::Fails), GO],
                operator_steps,
                &START_2,
                &[GO],
            ]),
        ))
    }

    #[test]
    fn first_property_holds_for_a_member_nobody_touched() {
        let after = refusal_then(&[]);
        assert!(matches!(started(&after).0, Started::Ran { .. }));
        assert!(holds_at(as_is(), STARTS_ONLY_OWED, &after));
    }

    #[test]
    fn first_property_fails_for_a_member_the_operator_held() {
        let after = refusal_then(&[operator(Pick::Stop)]);
        assert!(matches!(started(&after).0, Started::Ran { .. }));
        assert!(!holds_at(as_is(), STARTS_ONLY_OWED, &after));
    }

    #[test]
    fn first_property_fails_for_a_member_the_operator_started_then_stopped() {
        let after = refusal_then(&[operator(Pick::Go), operator(Pick::Stop)]);
        assert!(!holds_at(as_is(), STARTS_ONLY_OWED, &after));
    }

    /// Nobody stopped it, so the start's 304 is the benign case (bollard 0.21.0 maps it to Ok).
    #[test]
    fn first_property_holds_for_a_member_the_operator_only_started() {
        let after = refusal_then(&[operator(Pick::Go)]);
        assert_eq!(started(&after).0, Started::AlreadyRunning);
        assert!(holds_at(as_is(), STARTS_ONLY_OWED, &after));
    }

    #[test]
    fn first_property_holds_for_a_member_the_operator_removed() {
        let after = refusal_then(&[operator(Pick::Remove)]);
        assert_eq!(started(&after).0, Started::Gone);
        assert!(holds_at(as_is(), STARTS_ONLY_OWED, &after));
    }

    #[test]
    fn second_property_fails_for_a_detached_member_with_an_address_noted() {
        let after = step(
            as_is(),
            &at_start(as_is(), Endpoint::Detached, Some(Addr::A)),
            Pick::Go,
        );
        assert!(!holds_at(as_is(), STARTS_ONLY_ON_BRIDGE, &after));
    }

    /// `Pins::record` skips a member with no address (pins.rs:69), so none is noted.
    #[test]
    fn second_property_fails_for_a_detached_member_with_no_address_noted() {
        let after = step(
            as_is(),
            &at_start(as_is(), Endpoint::Detached, None),
            Pick::Go,
        );
        assert!(!holds_at(as_is(), STARTS_ONLY_ON_BRIDGE, &after));
    }

    /// Risk 3: no property, a row.
    #[test]
    fn second_property_holds_for_a_member_attached_on_another_address_than_owed() {
        let after = step(
            as_is(),
            &at_start(as_is(), Endpoint::Dynamic(Addr::B), Some(Addr::A)),
            Pick::Go,
        );
        assert!(holds_at(as_is(), STARTS_ONLY_ON_BRIDGE, &after));
        let risk_3 = row(
            "Stopped::start starts a member attached on a dynamic address while the address note owes another",
        );
        assert!((risk_3.reached)(&as_is(), &after));
    }

    #[test]
    fn second_property_holds_for_a_pinned_member() {
        let after = step(
            as_is(),
            &at_start(as_is(), Endpoint::Pinned(Addr::A), None),
            Pick::Go,
        );
        assert!(holds_at(as_is(), STARTS_ONLY_ON_BRIDGE, &after));
    }

    // The fix's start predicate at each state it must handle.

    #[test]
    fn fix_starts_a_start_debt_attached() {
        let model = fixed(OnRefusal::Drop);
        let before = at_start(model, Endpoint::Pinned(Addr::A), None);
        assert_eq!(debt(&before, Member::M0), Some(MemberDebt::Start));
        let after = step(model, &before, Pick::Go);
        assert!(matches!(
            started(&after),
            (
                Started::Ran {
                    detached: false,
                    ..
                },
                false
            )
        ));
    }

    #[test]
    fn fix_defers_a_start_debt_detached() {
        let model = fixed(OnRefusal::Drop);
        let before = at_start(model, Endpoint::Detached, None);
        assert_eq!(debt(&before, Member::M0), Some(MemberDebt::Start));
        let after = step(model, &before, Pick::Go);
        assert_eq!(started(&after), (Started::Deferred, false));
        assert_eq!(after.docker.m0.run, Run::Exited);
    }

    #[test]
    fn fix_starts_an_address_then_start_debt_attached_at_the_noted_address() {
        let model = fixed(OnRefusal::Drop);
        let before = at_start(model, Endpoint::Pinned(Addr::A), Some(Addr::A));
        assert_eq!(
            debt(&before, Member::M0),
            Some(MemberDebt::AddressThenStart)
        );
        let after = step(model, &before, Pick::Go);
        assert!(matches!(
            started(&after).0,
            Started::Ran {
                detached: false,
                ..
            }
        ));
    }

    #[test]
    fn fix_defers_an_address_then_start_debt_detached() {
        let model = fixed(OnRefusal::Retry);
        let before = at_start(model, Endpoint::Detached, Some(Addr::A));
        let after = step(model, &before, Pick::Go);
        assert_eq!(started(&after), (Started::Deferred, true));
        assert_eq!(after.pass.kept_why.m0, Why::Deferred);
    }

    /// The fix does not check the address: risk 3's state is started, and stays a row.
    #[test]
    fn fix_starts_an_address_then_start_debt_attached_on_another_address() {
        let model = fixed(OnRefusal::Drop);
        let before = at_start(model, Endpoint::Dynamic(Addr::B), Some(Addr::A));
        let after = step(model, &before, Pick::Go);
        assert!(matches!(
            started(&after).0,
            Started::Ran {
                detached: false,
                ..
            }
        ));
        assert!(holds_at(model, STARTS_ONLY_ON_BRIDGE, &after));
    }

    #[test]
    fn fix_handles_an_address_then_start_debt_whose_address_another_member_holds() {
        let model = BridgeNotes {
            members: 2,
            ..fixed(OnRefusal::Drop)
        };
        let mut before = at_start(model, Endpoint::Detached, Some(Addr::A));
        before.docker.m1.endpoint = Endpoint::Dynamic(Addr::A);
        let after = step(model, &before, Pick::Go);
        assert_eq!(started(&after).0, Started::Deferred);

        before.docker.m0.endpoint = Endpoint::Dynamic(Addr::C);
        let after = step(model, &before, Pick::Go);
        assert!(matches!(
            started(&after).0,
            Started::Ran {
                detached: false,
                ..
            }
        ));
    }

    #[test]
    fn fix_drops_or_retries_a_start_docker_refuses() {
        let before = at_start(as_is(), Endpoint::Pinned(Addr::A), None);
        let dropped = step(fixed(OnRefusal::Drop), &before, Pick::Fails);
        assert_eq!(started(&dropped), (Started::Refused, false));

        let retried = step(fixed(OnRefusal::Retry), &before, Pick::Fails);
        assert_eq!(started(&retried), (Started::Refused, true));
        let updated = step(fixed(OnRefusal::Retry), &retried, Pick::Go);
        assert_eq!(updated.retries.m0, 1);
        assert!(updated.stopped.has(Member::M0));

        let mut spent = before;
        spent.retries.m0 = RETRIES;
        let dropped = step(fixed(OnRefusal::Retry), &spent, Pick::Fails);
        assert_eq!(started(&dropped), (Started::Refused, false));
    }

    #[test]
    fn fix_drops_or_retries_a_netless_refusal() {
        let before = at_start(as_is(), Endpoint::Pinned(Addr::A), None);
        let dropped = step(fixed(OnRefusal::Drop), &before, Pick::Netless);
        assert_eq!(started(&dropped), (Started::Netless, false));
        let retried = step(fixed(OnRefusal::Retry), &before, Pick::Netless);
        assert_eq!(started(&retried), (Started::Netless, true));
    }

    #[test]
    fn fix_drops_a_gone_container() {
        for on_refusal in [OnRefusal::Drop, OnRefusal::Retry] {
            let mut before = at_start(as_is(), Endpoint::Detached, None);
            before.docker.m0.run = Run::Gone;
            let after = step(fixed(on_refusal), &before, Pick::Go);
            assert_eq!(started(&after), (Started::Gone, false));
        }
    }

    // The findings' replays on the fix.

    /// Drop drops the refused id, so start 2 has nothing to start. Retry keeps it, and runs
    /// it at start 2 though the operator stopped it.
    #[test]
    fn fix_replays_the_refusal() {
        let path = follow(fixed(OnRefusal::Drop), &the_refusal());
        assert!(branches(&path).contains(&Branch::Start {
            member: Member::M0,
            answer: Started::Refused,
            why: None,
            operator_stopped: false,
            kept: false,
        }));
        let after = last(&path);
        assert!(after.stopped.is_empty());
        assert_eq!(after.docker.m0.run, Run::Exited);

        let path = follow(fixed(OnRefusal::Retry), &the_refusal());
        assert!(ran_after_an_operator_stop(&last(&path), Why::Refused));
    }

    /// Route 3 does not pass through the policy: it breaks the first property under both.
    #[test]
    fn fix_replays_the_failed_update() {
        for on_refusal in [OnRefusal::Drop, OnRefusal::Retry] {
            let path = follow(fixed(on_refusal), &the_failed_update());
            assert!(ran_after_an_operator_stop(&last(&path), Why::Unupdated));
        }
    }

    #[test]
    fn fix_replays_the_early_return() {
        for on_refusal in [OnRefusal::Drop, OnRefusal::Retry] {
            let model = fixed(on_refusal);
            let after = last(&follow(model, &the_early_return()));
            assert_eq!(started(&after).0, Started::Deferred);
            assert!(holds_at(model, STARTS_ONLY_ON_BRIDGE, &after));
        }
    }

    #[test]
    fn fix_replays_the_false_rejoin() {
        for on_refusal in [OnRefusal::Drop, OnRefusal::Retry] {
            let model = fixed(on_refusal);
            let after = last(&follow(model, &the_false_rejoin()));
            assert_eq!(started(&after).0, Started::Deferred);
            assert!(holds_at(model, STARTS_ONLY_ON_BRIDGE, &after));
        }
    }

    // Each flag removes one guard.

    #[test]
    fn stop_before_note_breaks_the_note_before_the_stop() {
        let model = broken(
            Variant::Shipped,
            Broken {
                stop_before_note: true,
                ..Broken::default()
            },
        );
        let found = breaks(model, NOTED_BEFORE_STOP);
        assert!(matches!(
            last(&found).took,
            Some(Branch::StoppedIt {
                settle: Settle::StopAndNote,
                noted: false,
                ..
            })
        ));
    }

    #[test]
    fn detach_unrecorded_breaks_the_address_note_before_the_detach() {
        let model = broken(
            Variant::Shipped,
            Broken {
                detach_unrecorded: true,
                ..Broken::default()
            },
        );
        let found = breaks(model, ADDRESS_NOTED_BEFORE_DETACH);
        assert!(branches(&found).iter().any(|branch| matches!(
            branch,
            Branch::Recorded {
                write: Write::Failed,
                ..
            }
        )));
    }

    /// No property here needs the guard. Without it a failed detach no longer keeps the
    /// bridge, and the readdress removes it with the member still on it, or Docker refuses.
    #[test]
    fn replace_after_failed_detach_removes_a_bridge_a_member_is_on() {
        let model = broken(
            Variant::Shipped,
            Broken {
                replace_after_failed_detach: true,
                ..Broken::default()
            },
        );
        let checker = check(model);
        for property in [STARTS_ONLY_OWED, STARTS_ONLY_ON_BRIDGE] {
            assert!(checker.discovery(property).is_some(), "{property} holds");
        }
        for property in [NOTED_BEFORE_STOP, ADDRESS_NOTED_BEFORE_DETACH] {
            assert!(checker.discovery(property).is_none(), "{property} fails");
        }
        assert_eq!(
            unreached(model, &checker),
            ["a readdress detach fails, and the bridge is not replaced"]
        );
        assert!(reaches(model, |state| state.took
            == Some(Branch::Removed { attached: true })));
        assert!(reaches(model, |state| state.took == Some(Branch::RemoveRefused)));
        assert!(!reaches(as_is(), |state| matches!(
            state.took,
            Some(Branch::Removed { attached: true } | Branch::RemoveRefused)
        )));
    }

    /// No property here needs the guard. Two members: M0 is detached and owed A, and M1
    /// holds A on a dynamic address. Without the guard, `Pins::restore` gives M0 A anyway.
    #[test]
    fn restore_held_restores_an_address_another_member_holds() {
        let guarded = BridgeNotes {
            members: 2,
            ..as_is()
        };
        let mut before = guarded
            .init_states()
            .into_iter()
            .next()
            .expect("a start state");
        before.bridge = Bridge::New;
        before.docker.m0.run = Run::Exited;
        before.docker.m0.endpoint = Endpoint::Detached;
        before.docker.m1.endpoint = Endpoint::Dynamic(Addr::A);
        before.pins.m0 = Some(Addr::A);
        before.pass.pins = before.pins;
        before.process = Process::Starting(At::List(Phase::Pin));
        let seen = |state: &State| state.pass.seen.m0.and_then(|seen| seen.ipv4);

        assert_eq!(seen(&step(guarded, &before, Pick::Go)), None);
        let unguarded = BridgeNotes {
            broken: Broken {
                restore_held: true,
                ..Broken::default()
            },
            ..guarded
        };
        assert_eq!(seen(&step(unguarded, &before, Pick::Go)), Some(Addr::A));

        let checker = check(unguarded);
        println!(
            "two members, unguarded: {} states",
            checker.unique_state_count()
        );
        for property in [STARTS_ONLY_OWED, STARTS_ONLY_ON_BRIDGE] {
            assert!(checker.discovery(property).is_some(), "{property} holds");
        }
        for property in [NOTED_BEFORE_STOP, ADDRESS_NOTED_BEFORE_DETACH] {
            assert!(checker.discovery(property).is_none(), "{property} fails");
        }
        assert!(unreached(unguarded, &checker).is_empty());
    }

    #[test]
    fn netless_gone_never_starts_a_member_refused_for_its_network() {
        let model = broken(
            Variant::Shipped,
            Broken {
                netless_gone: true,
                ..Broken::default()
            },
        );
        let checker = check(model);
        assert_eq!(
            unreached(model, &checker),
            [
                "Docker answers the netless 404, and the member stays written down",
                "a member Docker refused for a missing network is started at a later daemon start",
            ]
        );
    }

    #[test]
    fn fix_starts_detached_breaks_the_second_property() {
        let model = broken(
            Variant::Proposed(OnRefusal::Drop),
            Broken {
                fix_starts_detached: true,
                ..Broken::default()
            },
        );
        assert!(ran_detached(&last(&breaks(model, STARTS_ONLY_ON_BRIDGE))));
    }

    #[test]
    fn fix_keeps_unpaid_breaks_the_first_property_by_a_refusal() {
        let model = broken(
            Variant::Proposed(OnRefusal::Drop),
            Broken {
                fix_keeps_unpaid: true,
                ..Broken::default()
            },
        );
        let found = breaks_where(model, STARTS_ONLY_OWED, off_route_3);
        assert!(ran_after_an_operator_stop(&last(&found), Why::Refused));
    }

    // The policies.

    /// Drop keeps every `always` property but the first property's route 3, and gives up the
    /// rows that start a member at a later daemon start after its start was refused or deferred.
    #[test]
    fn fix_with_drop_breaks_only_route_3_and_gives_up_the_named_rows() {
        let model = fixed(OnRefusal::Drop);
        let checker = check(model);
        println!("drop: {} states", checker.unique_state_count());
        for property in [
            STARTS_ONLY_ON_BRIDGE,
            NOTED_BEFORE_STOP,
            ADDRESS_NOTED_BEFORE_DETACH,
        ] {
            assert!(checker.discovery(property).is_none(), "{property} fails");
        }
        assert!(checker.discovery(STARTS_ONLY_OWED).is_some());
        assert!(
            checker
                .discovery(
                    "Stopped::start starts a member the operator stopped, kept because the note's update failed"
                )
                .is_some()
        );
        assert!(first_break(model, STARTS_ONLY_OWED, off_route_3, false).is_none());
        assert!(
            checker
                .discovery(
                    "a member Ployz stopped is started at a later daemon start after a start that died"
                )
                .is_some()
        );
        assert_eq!(
            unreached(model, &checker),
            [
                "Docker refuses a start, and the member stays written down",
                "Docker answers the netless 404, and the member stays written down",
                "Stopped::start rewrites the stopped note with the members Docker did not start",
                "a member Docker refused is started at a later daemon start",
                "a member Docker refused for a missing network is started at a later daemon start",
                "Docker attaches a detached member it starts on a dynamic address while the address note owes another",
                "the fix drops a member whose retries are spent",
                "a member deferred for its address is started at a later daemon start",
            ]
        );
    }

    /// Retry keeps the rows Drop gives up, and breaks the first property by a refusal too.
    #[test]
    fn fix_with_retry_breaks_the_first_property_by_a_refusal() {
        let model = fixed(OnRefusal::Retry);
        let checker = check(model);
        println!("retry: {} states", checker.unique_state_count());
        for property in [
            STARTS_ONLY_ON_BRIDGE,
            NOTED_BEFORE_STOP,
            ADDRESS_NOTED_BEFORE_DETACH,
        ] {
            assert!(checker.discovery(property).is_none(), "{property} fails");
        }
        let found = breaks_where(model, STARTS_ONLY_OWED, off_route_3);
        assert!(ran_after_an_operator_stop(&last(&found), Why::Refused));
        assert_eq!(
            unreached(model, &checker),
            [
                "Docker attaches a detached member it starts on a dynamic address while the address note owes another",
            ]
        );
    }
}
