//! A person's yes, weighed against the plan and the keys as they stand when its effect
//! runs. One fault has been fixed again and again across Engine and Cloud: a yes checked
//! against a different plan, or at a different moment, than the write it allowed. This
//! model replays each of those fixes and checks them all against one property.
//!
//! The property: **every effect runs the plan a valid yes was given for**. A request that
//! carries a yes writes exactly the plan the person saw when they said yes, and only while
//! the yes still counts: from a person, never an agent; in a Protected Environment, from
//! someone who may answer its ask; on a Machine, from a key it still accepts when the
//! effect runs. A yes to a plan that changed since asks again, even when the plan now
//! destroys nothing (`Approval::carried`, `ployz_store::trusted`).
//!
//! The actors:
//!
//! - **The asker** is folded into the start: the person saw `Plan::Asked` in an ask and said
//!   yes, so the request carries that plan's digest. `Signer` says who carries it.
//! - **The editor** changes the plan once, at any step before the request ends: a
//!   collaborator edits or discards the draft, a Deployment lands so a retry or a cancel
//!   reads another version, every Server leaves, or the drained Server leaves the frame
//!   (`ApprovalBinding::edits`). With `open_paths` it also makes the change the code
//!   doesn't bind: the cancelled Deployment's runner claims it (`Plan::Started`).
//! - **The drained Server** changes under the Drain, each once. A user Global lands on it
//!   (`Action::Global`) until the Engine's second read: a Service the Drain's scope chose,
//!   redeployed as a Global, or another Service, which a Full caller's Global spec or a
//!   Deploy racing the Drain puts there. It stops answering Cloud's probes before Cloud's
//!   last compare (`Action::Silent`), so the frame lists it `unanswered` with its rows as
//!   it last reported them. It stops answering the Engine's listing before the Engine's
//!   second read (`Action::Unlisted`). Cloud and the Engine observe it apart (bet 1).
//! - **The frame** changes what `dataCheck` reads of a Service the Engine leaves on the
//!   drained Server for its Volume, once (`Data`): the Volume's reason to stay clears, the
//!   Service now mounts another managed Volume, or a Deploy made it a Global. Once a move is
//!   planned, a Full caller replaces the Service that holds the Volume with a Global, once,
//!   before admission or before the Move's cut (`Action::Swap`).
//! - **The remover** revokes the caller's management access on a Machine once
//!   (`Action::Revoke`): a Full caller's `set_device` removes the Device and its slot under
//!   its own exclusive turn (`machine/local_machine/management_client.rs`).
//! - **The executor** sends the request (`Action::Send`), and where the code splits it, takes
//!   its later steps: Cloud's second Store call before f06d2a6351 (`Action::Resend`), a
//!   Drain's compare and Inngest's retry of it (`Action::Run`, `Action::Crash`), then its
//!   Engine call: the first read and services off (`Action::Engine`), the second read
//!   (`Action::Reread`), then its Volume plan (`Action::Volumes`), each move's
//!   admission (`Action::Admit`) and its Move's cut (`Action::Cut`); and a Machine's
//!   admitted turn (`Action::Run`).
//!
//! Each gate is restated by hand from the function it copies, with each shortcut that once
//! skipped the digest comparison as a `Broken` flag that restores it:
//!
//! - Cloud's Config API (`dashboard/src/routes/api/config/-config.handler.ts`,
//!   `handleConfigRequest`) refuses an agent carrying a yes, then sends a person's yes into
//!   the Store's write (`Store` commands).
//! - The Store weighs the yes inside that write, under the Environment's lock, so the
//!   editor's write lands before or after it and never inside (`Write`, each naming its
//!   gate in `ployz-store`).
//! - Cloud's `gateOperation` (`dashboard/src/modules/operations/gate.server.ts`) weighs a
//!   Drain's yes when it is asked, and `stillCovered`
//!   (`dashboard/src/modules/machines/server-drain.server.ts`) plans again each time
//!   `executeDrainOnce` runs. Both refuse, through `drainRefusal`
//!   (`server-operations.server.ts`), a Server the frame lists `unanswered` or one running a
//!   user Global, and `stillCovered` refuses one the frame lost. It hands the Engine only
//!   the Services in scope: `drain_machine` (`ployz/src/drain.rs`, `preflight`) lists the
//!   Server before and after it turns services off, and `chosen_on` refuses at either read
//!   a Server that didn't answer or that runs a user Global, chosen or not. Later still,
//!   `planDrainVolumes` (`dashboard/src/modules/machines/server-drain-volumes.server.ts`)
//!   plans each Volume move from a new frame through `dataCheck`, which keeps a Service any
//!   copy of which is Global, and `admitDrainVolume` admits or refuses it. Admission and
//!   each Move step read the holder again: `holderOn` (`volume-run.cluster.server.ts`)
//!   refuses a Global's copy, and the Drain's pin (`admitDrainVolume`'s compare,
//!   `holderSwapped` in `volume-run.inngest.ts`) a Container other than the one it chose
//!   (`Command::Drain`). A Global that replaced the holder is a new Container, so either
//!   refuses it; the model folds them into one holder check.
//!   The carried yes's ask shows the Drain's effects and no preview (`yesRequired`'s
//!   `previewed`), so it names no Volume forecast, and the model checks no move against one.
//! - A Machine checks the caller's management access, `require_management_access`
//!   (`ployzd/src/machine/local_machine.rs`), in `admit_installation`
//!   (`ployzd/src/machine/local_machine/turns.rs`). `finish_mutation` and `finish_shared`
//!   take their turn first and check under it; `finish_installed` checks when the call
//!   arrives and again under each `exclusive_turn` its work takes later
//!   (`Installed::exclusive_turn`, `checked_under`) (`Command::Machine`).
//!
//! Open paths, which `open_paths` adds and the shipped check leaves out: a Cancel's digest
//! binds the Deployment and the review version, which stay put when its runner claims it,
//! so a yes to taking a queued Deployment out of the queue stops one already running
//! (#4555).
//!
//! A Drain retires no Global. Up to #4567 it retired each user Global's copy on the drained
//! Server past `last_in`, which counts a crash-looping copy elsewhere In, so it could take
//! the last copy that served. A Full caller still makes a user Global (the Machine API
//! admits a resolved Global spec, `admission.rs`, `Scope::Full`), and Compose did up to
//! v0.1.2-beta.41, so the Drain refuses any user Global on its Server; a system Global it
//! never touches (`DrainScope::includes`). The Engine's refusal is the one that holds: it
//! reads the Server itself, after Cloud's last compare. Cloud's refusal of a Global it
//! reads only refuses earlier, before a yes, so no `Broken` flag removes it; the
//! dashboard's Postgres tests cover it. A Global or a listing lost between the Engine's two
//! reads refuses after services went off: the refusal says so, and the model counts it as
//! no effect, as bet 4 reports a prefix.
//!
//! Out of scope: a yes the dashboard gives itself (a dashboard confirm, including the Drain
//! dialog's Volume forecast, `previewServerDrain`), a Deploy that waits
//! at an approval link, the proposal decision that answers a Protected ask, an Organization
//! with no approver (Cloud then sends `not_required`), what a yes or its absence lets
//! through when it is not carried, a Project-wide removal, Cloud's other planned
//! operations (`cleanNamespace`, `setServer`'s ingress-off), a Container creation's
//! signed version (`AdmittedSpec::still_holds`, checked under the exclusive turn), and
//! Device enforcement switching on, which no longer exists: since a03ef88eea every daemon
//! enforces.

use std::fmt::Write as _;

use stateright::{Model, Property};

/// A Store write a CLI sends through Cloud's Config API, each with the gate the Store runs
/// for it inside the write.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Write {
    /// `ployz save`: `review::publish::publish`, then `review::approve` with
    /// `Shipping::Publish`. Nothing staged returns early (`publish.rs`, the nothing-staged return).
    Save,
    /// A Deploy carrying a yes: Cloud sends it to the Store instead of a link, and
    /// `review::approve` weighs it with `Shipping::Deploy`.
    Deploy,
    /// The retry of a Deployment that ships: `deployment::admit`'s `Admit::Retry`, then
    /// `review::approve_retry` under `Gate::retrying`.
    Retry,
    /// The retry of a removal Deployment: `admit`'s `Asked::Removal` arm
    /// (`deployment/admit.rs:241-255`), `removal::ask_taking_off`, `approve_shut_down`.
    RemovalRetry,
    /// A shutdown: `removal::shut_down` (`removal/due.rs:708`), `ask_taking_off`,
    /// `approve_shut_down`.
    ShutDown,
    /// An Environment's removal: `removal::remove`, then `removal::gate`
    /// (`removal/mod.rs:753`).
    Remove,
    /// Cancelling a Deployment in flight: `removal::cancel` (`removal/mod.rs:862`), then
    /// `review::approve_cancel`.
    Cancel,
    /// A Sync or Take: `review::move_asks` before it writes, `approve_move` after, in one
    /// transaction.
    Move,
}

/// How a Machine runs a management client's change.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Route {
    /// `finish_mutation` or `finish_shared`: the turn first, then `admit_installation`
    /// checks the caller's access under it.
    Turned,
    /// `finish_installed`: `admit_installation` checks the caller's access when the call
    /// arrives, and the work takes `Installed::exclusive_turn` itself after its drain, which
    /// checks the caller again under it: `ContainerStep::StopThenRemove` and
    /// `ContainerStep::Remove` (`machine/local_machine/container.rs`), `stop_checked`'s
    /// stop, and a Volume cut.
    Installed,
}

/// What the executor sends.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Command {
    Store(Write),
    /// `ployz server drain`: `drainServer` (`modules/operations/operations.server.ts`)
    /// runs `gateOperation` and records the plan's digest; `executeDrainOnce` runs
    /// `stillCovered` against the plan as it stands then.
    Drain,
    Machine(Route),
}

impl Command {
    const ALL: [Self; 11] = [
        Self::Store(Write::Save),
        Self::Store(Write::Deploy),
        Self::Store(Write::Retry),
        Self::Store(Write::RemovalRetry),
        Self::Store(Write::ShutDown),
        Self::Store(Write::Remove),
        Self::Store(Write::Cancel),
        Self::Store(Write::Move),
        Self::Drain,
        Self::Machine(Route::Turned),
        Self::Machine(Route::Installed),
    ];

    /// The plans the editor may move this command's plan to, among the ones the code's
    /// digest binds.
    const fn edits(self) -> &'static [Plan] {
        match self {
            Self::Store(Write::Save) => &[Plan::Other, Plan::Harmless, Plan::Discarded],
            Self::Store(Write::Deploy | Write::Retry | Write::Remove) => {
                &[Plan::Other, Plan::Harmless]
            }
            Self::Store(Write::RemovalRetry | Write::ShutDown) => {
                &[Plan::Other, Plan::Harmless, Plan::Forgets]
            }
            Self::Store(Write::Cancel | Write::Move) => &[Plan::Other],
            Self::Drain => &[Plan::Other, Plan::Harmless, Plan::Gone],
            Self::Machine(_) => &[],
        }
    }

    const fn machine(self) -> bool {
        matches!(self, Self::Machine(_))
    }
}

/// A plan, as its digest binds it.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Plan {
    /// What the person saw when they said yes. It destroys something.
    Asked,
    /// An edit since that destroys something else: another draft, another rollback, a
    /// version moved by a Deployment, a source at a newer revision.
    Other,
    /// An edit since that destroys nothing, or ships already-Saved State.
    Harmless,
    /// Nothing staged: the draft the yes was given to was discarded, so Working State is
    /// Head again.
    Discarded,
    /// Every Server left, so `deployment::forgettable` answers `Forgotten` and the Store
    /// only forgets what ran. It names the same effects as `Asked`.
    Forgets,
    /// The drained Server left the frame, so `drainPlan` answers null.
    Gone,
    /// The cancelled Deployment's runner claimed it: `Queued` became `Running`, so the
    /// cancel stops it partway instead of taking it out of the queue (`deployment::cancel`).
    /// Head's token counts only Deployments that ended (`history::head`), so the review
    /// version, and with it the digest, stays the asked one's.
    Started,
    /// A user Global runs on the drained Server. Only an effect: the Drain turned services
    /// off around a copy that may be the Global's last that works, or a Volume move cut it.
    Global,
}

impl Plan {
    /// Whether the plan names a destructive effect.
    const fn destroys(self) -> bool {
        match self {
            Self::Asked | Self::Other | Self::Forgets | Self::Started | Self::Global => true,
            Self::Harmless | Self::Discarded | Self::Gone => false,
        }
    }

    /// The digest the code binds for this plan, written as the plan it names.
    /// `approve_shut_down` binds `forgets`; without it, forgetting reads as taking the
    /// same Services off. Nothing binds a Deployment's start.
    const fn digest(self, broken: Broken) -> Self {
        match self {
            Self::Forgets if broken.forget_unbound => Self::Asked,
            Self::Started => Self::Asked,
            Self::Asked
            | Self::Other
            | Self::Harmless
            | Self::Discarded
            | Self::Forgets
            | Self::Gone
            | Self::Global => self,
        }
    }
}

/// What an Environment's protection asks of the caller (`review::approval::Protection`).
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Protection {
    /// Not Protected, or the caller may protect it: an owner or admin.
    Unasked,
    /// Protected, and the caller is a member who may not protect it.
    Asks,
    /// A protector's token: nothing asks, but `Gate::retrying` makes it
    /// `AsksUnlessHarmless`.
    Exempt,
    /// A protector's retry: it asks unless it destroys nothing.
    AsksUnlessHarmless,
}

impl Protection {
    /// `Protection::asks`, which `Gate::protected` reads.
    const fn asks(self) -> bool {
        match self {
            Self::Asks | Self::AsksUnlessHarmless => true,
            Self::Unasked | Self::Exempt => false,
        }
    }

    /// `Gate::retrying`.
    const fn retrying(self) -> Self {
        match self {
            Self::Unasked => Self::Unasked,
            Self::Exempt => Self::AsksUnlessHarmless,
            Self::Asks | Self::AsksUnlessHarmless => Self::Asks,
        }
    }
}

/// Who carries the yes.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Signer {
    /// A person, with `ployz`.
    Person,
    /// An agent token, which echoes the digest an ask showed it.
    Agent,
}

/// What stays fixed while the request runs.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct Facts {
    protection: Protection,
    /// The cancelled Deployment is someone else's (`removal::cancel`'s `theirs`).
    theirs: bool,
    signer: Signer,
}

/// What the Store's gate holds from Cloud (`ployz_store::trusted::Approval`, without
/// `NotRequired`, which only an Organization with no approver sends).
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Approval {
    Required,
    Approved(Plan),
}

impl Approval {
    /// `Approval::carried`.
    const fn carried(self) -> bool {
        matches!(self, Self::Approved(_))
    }
}

/// The Store's ask: `approval_required` with the digest a yes must name.
#[derive(Clone, Copy, Debug)]
struct Ask {
    digest: Plan,
    /// The ask's `protected` detail, which Cloud reads with `isProtectedAsk`.
    protected: bool,
}

/// Why the request ended without an effect.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Why {
    /// Cloud refused an agent's yes.
    Agent,
    /// The gate asked again for the plan as it stands.
    Asked,
    /// Cloud answered a Protected ask "An owner or admin does this."
    Protected,
    /// `drainPlan` found no Server to drain when it was asked.
    NoPlan,
    /// `stillCovered` found the Drain changed since its gate.
    Changed,
    /// `drainRefusal`: the frame lists the drained Server `unanswered`, or `stillCovered`
    /// found it gone from the frame.
    Unseen,
    /// `drainRefusal`: the frame shows a user Global on the drained Server.
    Global,
    /// The Engine's `chosen_on` read a user Global on the drained Server.
    EngineGlobal,
    /// The Engine's `chosen_on` got no listing from the drained Server (`Unobservable`).
    Unobservable,
    /// `ManagementRevoked`: no slot accepts the caller's key.
    Revoked,
}

/// The executor's progress.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Exec {
    Ready,
    /// Cloud's first Store call asked for exactly the digest the yes names, so its second
    /// call carries the yes (before f06d2a6351).
    Resending,
    /// The Drain's row holds the digest its gate weighed, or the Machine admitted the call,
    /// and its run is still to come.
    Waiting {
        digest: Plan,
    },
    /// `stillCovered` matched the row's digest; the Engine is still to be asked.
    Covered {
        digest: Plan,
    },
    /// The Engine's first read passed and it turned services off; its second read is still
    /// to come.
    Off,
    /// The Engine drained and answered; the Volume moves are still to be planned.
    Moving,
    /// `planDrainVolumes` planned a move from what `dataCheck` read; `admitDrainVolume` is
    /// still to admit or refuse it.
    Admitting {
        read: Data,
    },
    /// `admitDrainVolume` admitted the move; its Move's steps are still to read the holder
    /// again and cut it.
    Cutting {
        read: Data,
    },
    Ran,
    /// Refused, `waited` once admitted, at the run.
    Refused {
        why: Why,
        waited: bool,
    },
}

/// The effect a request wrote.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct Wrote {
    plan: Plan,
    /// The yes counted when the effect ran: a person's, one the Protected Environment
    /// takes, from a key the Machine still accepts.
    valid: bool,
}

/// What `dataCheck` reads of a Service the Engine left on the drained Server for its one
/// managed Volume. `operationDigest` binds none of it, and the carried yes's ask shows none.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Data {
    /// It stays, as when the yes was asked: it shares its Volume, or no single Container runs
    /// on it.
    Stays,
    /// The reason cleared: the sharer stopped, or its one Container started.
    Frees,
    /// The Service now mounts another managed Volume no one shares, deployed onto the
    /// drained Server before the Engine turned its services role off (`dataCheck` reads the
    /// newest Container).
    Switched,
    /// A Full caller replaced the Service with a Global after the Engine left it, so its
    /// Container on the Volume is a Global's copy.
    Global,
}

/// The step a Global that replaced the Volume's planned holder landed before: a Full
/// caller replaced the Service with a Global after `planDrainVolumes` read it.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Swap {
    /// `admitDrainVolume`, which reads the holder through `holderOn` (`readHolder`).
    Admit,
    /// The Move's steps, which read it through `holderOn` (`findHolder`) before the cut.
    Cut,
}

/// The Drain step a change to the drained Server landed before.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Before {
    /// Cloud's gate, which reads the frame when the request arrives.
    Gate,
    /// Cloud's last compare, `stillCovered`.
    Compare,
    /// The Engine's first read, before it turns services off.
    Read,
    /// The Engine's second read, after services went off.
    Reread,
}

/// A user Global on the drained Server.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct Landed {
    /// It is a Service the Drain's scope chose, redeployed as a Global. Cloud saves the
    /// scope at the request, so any other Service that lands later is outside it.
    chosen: bool,
    before: Before,
}

/// What Cloud's `drainPlan` reads of the drained Server.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct Seen {
    plan: Plan,
    /// A user Global runs there.
    global: bool,
}

/// What `admitDrainVolume` did with a planned move.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Admission {
    /// It admitted the move of the Volume `planDrainVolumes` read.
    Moved(Data),
    /// Every candidate refused, the target stopped taking services, or the drain no longer
    /// owns its row: the Volume stays, with that reason on the report.
    Stayed,
    /// The holder check refused the move: a Global's copy holds the Volume, or a Container
    /// other than the one the Drain chose. The Volume stays.
    HolderRefused,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct State {
    facts: Facts,
    /// The plan as it stands.
    live: Plan,
    /// The editor made its one change.
    edited: bool,
    /// The Machine accepts the caller's key.
    trusted: bool,
    /// The Drain's step died once before its effect, and Inngest ran it again.
    crashed: bool,
    global: Option<Landed>,
    /// The drained Server stopped answering Cloud's probes; what Cloud read of it last.
    silent: Option<Seen>,
    /// The drained Server stopped answering the Engine's listing.
    unlisted: bool,
    /// The Engine turned the drained Server's services off.
    off: bool,
    data: Data,
    /// The frame changed `data` after Cloud's last compare.
    data_late: bool,
    /// A Global replaced the Volume's planned holder.
    swapped: Option<Swap>,
    exec: Exec,
    wrote: Option<Wrote>,
    admission: Option<Admission>,
}

impl State {
    const fn over(&self) -> bool {
        matches!(self.exec, Exec::Ran | Exec::Refused { .. })
    }

    /// Cloud compared the Drain's plan for the last time.
    const fn compared(&self) -> bool {
        matches!(
            self.exec,
            Exec::Covered { .. }
                | Exec::Off
                | Exec::Moving
                | Exec::Admitting { .. }
                | Exec::Cutting { .. }
                | Exec::Ran
        )
    }

    /// The Drain step a change to the drained Server lands before now; none once the
    /// Engine's second read passed.
    const fn window(&self) -> Option<Before> {
        match self.exec {
            Exec::Ready => Some(Before::Gate),
            Exec::Resending | Exec::Waiting { .. } => Some(Before::Compare),
            Exec::Covered { .. } => Some(Before::Read),
            Exec::Off => Some(Before::Reread),
            Exec::Moving
            | Exec::Admitting { .. }
            | Exec::Cutting { .. }
            | Exec::Ran
            | Exec::Refused { .. } => None,
        }
    }

    fn refuse(&mut self, why: Why) {
        let waited = matches!(self.exec, Exec::Waiting { .. } | Exec::Resending);
        self.exec = Exec::Refused { why, waited };
    }
}

/// A guard taken out, to replay the code before the commit that added it.
#[derive(Clone, Copy, Debug, Default, Hash, PartialEq, Eq)]
struct Broken {
    /// f06d2a6351: Cloud's first Store call carries no yes, and only after an ask naming
    /// the yes's digest does a second call carry it.
    cloud_calls_twice: bool,
    /// f06d2a6351: `review::approve`'s shortcut for a Save or Deploy that destroys nothing
    /// lets it through without weighing a carried yes.
    approve_skips_yes: bool,
    /// 0f29e0d3f3: a Save with nothing staged returns before any gate.
    empty_save_skips_yes: bool,
    /// 0f29e0d3f3: `approve_shut_down` binds no `forgets`.
    forget_unbound: bool,
    /// 0f29e0d3f3: `removal::cancel` weighs only someone else's Deployment that ships.
    cancel_skips_unless_theirs: bool,
    /// 8836d08f2f: `approve_retry`'s shortcut for a retry that asks nothing and destroys
    /// nothing.
    retry_skips_yes: bool,
    /// 8836d08f2f: `approve_retry`'s shortcut for a protector's retry that destroys
    /// nothing.
    protector_retry_skips_yes: bool,
    /// 8836d08f2f: `shut_down` and `admit`'s removal retry ask nothing when the Store only
    /// forgets.
    forget_skips_yes: bool,
    /// 8836d08f2f: `approve_shut_down`'s shortcut for nothing left to take off.
    shutdown_skips_yes: bool,
    /// 8836d08f2f: `removal::gate`'s shortcut for a removal that destroys nothing.
    remove_skips_yes: bool,
    /// 8836d08f2f: `approve_cancel` returns in an Environment that isn't Protected for the
    /// caller.
    cancel_skips_unprotected: bool,
    /// 8836d08f2f: `move_asks` answers nothing outside a Protected Environment.
    move_skips_yes: bool,
    /// 1c93248b9b: the Config API takes an agent's yes as a person's.
    agent_yes_unrefused: bool,
    /// 1c93248b9b: `gateOperation` lets a plan with no effects through before it compares a
    /// carried digest.
    gate_skips_yes: bool,
    /// 1c93248b9b: `stillCovered` covers any Drain that destroys nothing, yes or not.
    drain_runs_harmless: bool,
    /// bad55e1aed: `stillCovered` covers a Drain whose Server left the frame, yes or not.
    drain_runs_gone: bool,
    /// 273b77fabc: a turn runs what its call's arrival admitted, without checking the
    /// caller again.
    turn_trusts_arrival: bool,
    /// #4558: the work `finish_installed` admitted takes `exclusive_turn` after its drain
    /// and runs without checking the caller again under it.
    installed_trusts_arrival: bool,
    /// A guard f06d2a6351 needed, not a past bug: `Gate::ask` takes a member's own CLI yes
    /// for what a Protected Environment asks of an owner or admin.
    member_yes_answers_protected: bool,
    /// #4567: the Engine drained a Server running a user Global, retiring it, as `main` did
    /// before. Cloud's refusal still holds for one it reads.
    engine_skips_globals: bool,
    /// #4567 before its fourth round: `chosen_on` looked for a Global only among the
    /// Services the Drain's scope chose.
    engine_global_scoped: bool,
    /// A guard, not a past bug: `chosen_on` refuses a Server whose listing didn't answer
    /// (`observed`). Set, it reads that Server as running nothing.
    engine_reads_unlisted_empty: bool,
    /// #4567 before its fourth round: `drainPlan` read a Server the frame lists
    /// `unanswered` from the rows it last reported.
    cloud_reads_unanswered: bool,
    /// #4567 before its fourth round: `dataCheck` and `holderOn` took a Global's copy as
    /// the holder a Volume move cuts.
    volume_moves_global: bool,
    /// A guard, not a past bug: the holder check at admission and at each Move step, which
    /// refuses a Global's copy (`holderOn`) or a Container other than the one the Drain
    /// chose (`admitDrainVolume`'s compare, `holderSwapped`). A Global that replaced the
    /// holder is a new Container, so either refuses it. Set, the Move cuts whatever holds
    /// the Volume when its step reads it.
    move_skips_holder: bool,
}

#[derive(Clone, Copy, Debug)]
struct ApprovalBinding {
    command: Command,
    broken: Broken,
    /// The change the shipped code lets through: a Cancel's Deployment starts. It breaks the
    /// property; see the module doc.
    open_paths: bool,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Action {
    /// The editor moves the plan.
    Edit(Plan),
    /// A user Global lands on the drained Server, a Service the Drain's scope chose or not.
    Global { chosen: bool },
    /// The drained Server stops answering Cloud's probes.
    Silent,
    /// The drained Server stops answering the Engine's listing.
    Unlisted,
    /// The remover revokes the caller's management access.
    Revoke,
    /// The executor sends its request.
    Send,
    /// Cloud's second Store call, with the yes (before f06d2a6351).
    Resend,
    /// The Drain's `executeDrainOnce` up to `stillCovered`, or the Machine's admitted turn.
    Run,
    /// The Drain's step dies before the Engine is asked; Inngest runs it again from the top.
    Crash,
    /// The frame changes what `dataCheck` reads of the Service left for its Volume.
    Data(Data),
    /// `executeDrainOnce` calls `drainMachine` with the Services in scope; its `preflight`
    /// reads the drained Server through `chosen_on`, then turns services off.
    Engine,
    /// `preflight` reads the drained Server through `chosen_on` again, and the Engine drains
    /// and answers.
    Reread,
    /// `planDrainVolumes` reads a new frame and plans each move `dataCheck` allows.
    Volumes,
    /// `admitDrainVolume` admits the planned move (true) or refuses it (false), after its
    /// holder check.
    Admit(bool),
    /// A Full caller replaces the Service that holds the Volume with a Global, after
    /// `planDrainVolumes` planned its move and before the Move's cut.
    Swap,
    /// The Move's steps read the holder again through the holder check, then cut it.
    Cut,
}

impl ApprovalBinding {
    /// The plans the editor may move the plan to from `state`.
    const fn edits(&self, state: &State) -> &'static [Plan] {
        match (self.command, state.exec) {
            (Command::Store(Write::Cancel), _) if self.open_paths => &[Plan::Other, Plan::Started],
            // After Cloud's last compare, what changes is the drained Server
            // (`server_changes`) and the Volume read (`data_changes`).
            (Command::Drain, _) if state.compared() => &[],
            (command, _) => command.edits(),
        }
    }

    /// What may change on the drained Server from `state`: a user Global lands there until
    /// the Engine's second read, it stops answering Cloud's probes until Cloud's last
    /// compare, and it stops answering the Engine's listing until that second read.
    fn server_changes(&self, state: &State, actions: &mut Vec<Action>) {
        if self.command != Command::Drain {
            return;
        }
        let Some(window) = state.window() else {
            return;
        };
        if state.global.is_none() {
            actions.push(Action::Global { chosen: true });
            actions.push(Action::Global { chosen: false });
        }
        if state.silent.is_none() && matches!(window, Before::Gate | Before::Compare) {
            actions.push(Action::Silent);
        }
        if !state.unlisted {
            actions.push(Action::Unlisted);
        }
    }

    /// What the frame may change `dataCheck`'s read to from `state`: a reason to stay
    /// clears until `planDrainVolumes` reads, a Service's Volume switches only while the
    /// drained Server still takes services, before the Engine's preflight turns that off,
    /// and a Full caller makes the Service a Global once the Engine has left it.
    fn data_changes(&self, state: &State) -> &'static [Data] {
        if self.command != Command::Drain || state.data != Data::Stays {
            return &[];
        }
        match state.exec {
            Exec::Ready | Exec::Resending | Exec::Waiting { .. } | Exec::Covered { .. } => {
                &[Data::Frees, Data::Switched]
            }
            Exec::Off => &[Data::Frees],
            Exec::Moving => &[Data::Frees, Data::Global],
            Exec::Admitting { .. } | Exec::Cutting { .. } | Exec::Ran | Exec::Refused { .. } => &[],
        }
    }

    /// Whether the holder check refuses a move planned from `read`: the Volume's holder is a
    /// Global's copy (`holderOn`), or `swapped` replaced the Container the Drain chose
    /// (the compare in `admitDrainVolume`, `holderSwapped` in the Move).
    const fn holder_refuses(&self, read: Data, swapped: Option<Swap>) -> bool {
        if self.broken.move_skips_holder {
            return false;
        }
        swapped.is_some() || (matches!(read, Data::Global) && !self.broken.volume_moves_global)
    }

    /// `Gate::ask`'s comparison, at `protection` as the gate holds it.
    fn require(&self, protection: Protection, digest: Plan, approval: Approval) -> Result<(), Ask> {
        let member = protection == Protection::Asks && !self.broken.member_yes_answers_protected;
        let ask = Ask {
            digest,
            protected: protection.asks(),
        };
        match approval {
            Approval::Approved(yes) if yes == digest && !member => Ok(()),
            Approval::Approved(_) | Approval::Required => Err(ask),
        }
    }

    /// `review::approve` for a Save (`deploy` false) or a Deploy.
    fn approve(
        &self,
        facts: Facts,
        plan: Plan,
        approval: Approval,
        deploy: bool,
    ) -> Result<(), Ask> {
        let protected = facts.protection.asks();
        let every_deploy = protected || (deploy && facts.signer == Signer::Agent);
        let weighs = approval.carried() && !self.broken.approve_skips_yes;
        if !every_deploy && !weighs && !plan.destroys() {
            return Ok(());
        }
        self.require(facts.protection, plan.digest(self.broken), approval)
    }

    /// The Store's gate for `write` on `plan`, inside the write.
    fn store_gate(
        &self,
        write: Write,
        facts: Facts,
        plan: Plan,
        approval: Approval,
    ) -> Result<(), Ask> {
        let broken = self.broken;
        let carried = approval.carried();
        let digest = plan.digest(broken);
        let destroys = plan.destroys();
        let agent = facts.signer == Signer::Agent;
        match write {
            Write::Save if plan == Plan::Discarded => {
                if carried && !broken.empty_save_skips_yes {
                    self.approve(facts, plan, approval, false)?;
                }
                Ok(())
            }
            Write::Save => self.approve(facts, plan, approval, false),
            Write::Deploy => self.approve(facts, plan, approval, true),
            Write::Retry => {
                let protection = facts.protection.retrying();
                let asks_retry = protection.asks() || agent;
                if !asks_retry && !destroys && !(carried && !broken.retry_skips_yes) {
                    return Ok(());
                }
                if protection == Protection::AsksUnlessHarmless
                    && !destroys
                    && !(carried && !broken.protector_retry_skips_yes)
                {
                    return Ok(());
                }
                self.require(protection, digest, approval)
            }
            Write::RemovalRetry | Write::ShutDown => {
                let asks_retry = facts.protection.asks() || agent;
                let asks = write == Write::RemovalRetry && asks_retry;
                let forget = plan == Plan::Forgets;
                if forget && !asks && !(carried && !broken.forget_skips_yes) {
                    return Ok(());
                }
                if !destroys && !asks_retry && !(carried && !broken.shutdown_skips_yes) {
                    return Ok(());
                }
                self.require(facts.protection, digest, approval)
            }
            Write::Remove => {
                if !destroys && !(carried && !broken.remove_skips_yes) {
                    return Ok(());
                }
                match approval {
                    Approval::Approved(yes) if yes == digest => Ok(()),
                    Approval::Approved(_) | Approval::Required => Err(Ask {
                        digest,
                        protected: false,
                    }),
                }
            }
            Write::Cancel => {
                if !facts.theirs && !(carried && !broken.cancel_skips_unless_theirs) {
                    return Ok(());
                }
                if !facts.protection.asks() && !(carried && !broken.cancel_skips_unprotected) {
                    return Ok(());
                }
                self.require(facts.protection, digest, approval)
            }
            Write::Move => {
                if !facts.protection.asks() && !(carried && !broken.move_skips_yes) {
                    return Ok(());
                }
                self.require(facts.protection, digest, approval)
            }
        }
    }

    /// Whether the yes counts now, for an effect about to run on `state.live`: never an
    /// agent's; in a Protected Environment, never a member's own where its protection asks
    /// an owner or admin; on a Machine, only while it accepts the caller's key.
    fn valid(&self, state: &State) -> bool {
        let person = state.facts.signer == Signer::Person;
        match self.command {
            Command::Store(write) => {
                let member = state.facts.protection == Protection::Asks;
                person && !(member && protection_asks(write, state.facts, state.live))
            }
            Command::Drain => person,
            Command::Machine(_) => state.trusted,
        }
    }

    fn run(&self, state: &mut State) {
        state.wrote = Some(Wrote {
            plan: state.live,
            valid: self.valid(state),
        });
        state.exec = Exec::Ran;
    }

    /// `handleConfigRequest`, and the Store write or writes it makes.
    fn send_store(&self, state: &mut State, write: Write) {
        let broken = self.broken;
        // `byAgent && named !== null`: an agent can't say yes.
        if state.facts.signer == Signer::Agent && !broken.agent_yes_unrefused {
            state.refuse(Why::Agent);
            return;
        }
        let approval = if broken.cloud_calls_twice {
            Approval::Required
        } else {
            Approval::Approved(Plan::Asked)
        };
        match self.store_gate(write, state.facts, state.live, approval) {
            Ok(()) => self.run(state),
            Err(ask) if !broken.cloud_calls_twice => {
                state.refuse(if ask.protected {
                    Why::Protected
                } else {
                    Why::Asked
                });
            }
            Err(ask) if ask.protected => state.refuse(Why::Protected),
            // `named === asked`: the ask names the yes's digest, so call again with it.
            Err(ask) if ask.digest == Plan::Asked => state.exec = Exec::Resending,
            Err(_) => state.refuse(Why::Asked),
        }
    }

    /// What Cloud's `drainPlan` reads of the drained Server, or none when the frame lists it
    /// `unanswered`, which `drainRefusal` refuses. Under Broken, an unanswered Server reads
    /// as it last reported.
    fn cloud_reads(&self, state: &State) -> Option<Seen> {
        match state.silent {
            Some(seen) if self.broken.cloud_reads_unanswered => Some(seen),
            Some(_) => None,
            None => Some(Seen {
                plan: state.live,
                global: state.global.is_some(),
            }),
        }
    }

    /// `drainServer`'s `planDrain`, `drainRefusal` and `gateOperation`, then the row it
    /// records.
    fn send_drain(&self, state: &mut State) {
        let Some(seen) = self.cloud_reads(state) else {
            state.refuse(Why::Unseen);
            return;
        };
        let plan = seen.plan;
        if plan == Plan::Gone {
            state.refuse(Why::NoPlan);
            return;
        }
        if seen.global {
            state.refuse(Why::Global);
            return;
        }
        let agent = state.facts.signer == Signer::Agent;
        let digest = plan.digest(self.broken);
        let admitted = if self.broken.gate_skips_yes {
            // Before 1c93248b9b: no effects passed before anything else.
            !plan.destroys() || (!agent && digest == Plan::Asked)
        } else {
            !agent && digest == Plan::Asked
        };
        match (admitted, agent) {
            (true, _) => state.exec = Exec::Waiting { digest },
            (false, true) => state.refuse(Why::Agent),
            (false, false) => state.refuse(Why::Asked),
        }
    }

    /// `executeDrainOnce`'s `stillCovered`, for a Drain a person said yes to. Covered, it
    /// waits for the Engine call.
    fn run_drain(&self, state: &mut State, digest: Plan) {
        let broken = self.broken;
        let Some(seen) = self.cloud_reads(state) else {
            state.refuse(Why::Unseen);
            return;
        };
        let present = seen.plan != Plan::Gone;
        if !present && !broken.drain_runs_harmless && !broken.drain_runs_gone {
            state.refuse(Why::Unseen);
            return;
        }
        if present && seen.global {
            state.refuse(Why::Global);
            return;
        }
        let matches = present && seen.plan.digest(broken) == digest;
        let covered = if broken.drain_runs_harmless {
            !present || !seen.plan.destroys() || matches
        } else if broken.drain_runs_gone {
            // `plan === null || (plan.effects.length === 0 && !admitted.approved)`.
            !present || matches
        } else {
            matches
        };
        if covered {
            state.exec = Exec::Covered { digest };
        } else {
            state.refuse(Why::Changed);
        }
    }

    /// `preflight`'s `chosen_on`: it refuses a drained Server whose listing didn't answer,
    /// and one running a user Global, whatever the scope chose. The first read comes before
    /// services go off, the second (`second`) after. A Server Cloud's frame lost may still
    /// be one the Engine sees, so the Engine drains it too.
    fn engine_read(&self, state: &mut State, second: bool) {
        let broken = self.broken;
        if state.unlisted && !broken.engine_reads_unlisted_empty {
            state.refuse(Why::Unobservable);
            return;
        }
        let seen = !state.unlisted
            && state
                .global
                .is_some_and(|global| global.chosen || !broken.engine_global_scoped);
        if seen && !broken.engine_skips_globals {
            state.refuse(Why::EngineGlobal);
            return;
        }
        if !second {
            state.off = true;
            state.exec = Exec::Off;
            return;
        }
        let plan = if state.global.is_some() {
            Plan::Global
        } else {
            state.live
        };
        state.wrote = Some(Wrote {
            plan,
            valid: self.valid(state),
        });
        state.exec = Exec::Moving;
    }

    /// `planDrainVolumes`: the Service the Engine left for its Volume gets a move when the
    /// new frame's `dataCheck` allows one. It keeps a Service any copy of which is Global.
    fn drain_volumes(&self, state: &mut State) {
        state.exec = match state.data {
            Data::Stays => Exec::Ran,
            Data::Global if !self.broken.volume_moves_global => Exec::Ran,
            read @ (Data::Frees | Data::Switched | Data::Global) => Exec::Admitting { read },
        };
    }
}

/// Whether a Protected Environment's protection asks an owner or admin about `write` on
/// `plan`, as the code's doc comments state it: every Save and Deploy, every retry, a
/// removal's retry, a shutdown that takes Services off (not one that only forgets, where
/// nothing on a Server is left), a cancel of someone else's Deployment, and every Sync or
/// Take. A Protected Environment's removal is refused outright.
const fn protection_asks(write: Write, facts: Facts, plan: Plan) -> bool {
    match write {
        Write::Save | Write::Deploy | Write::Retry | Write::RemovalRetry | Write::Move => true,
        Write::ShutDown => !matches!(plan, Plan::Forgets),
        Write::Cancel => facts.theirs,
        Write::Remove => false,
    }
}

impl Model for ApprovalBinding {
    type State = State;
    type Action = Action;

    fn init_states(&self) -> Vec<State> {
        let protections: &[Protection] = match self.command {
            Command::Store(Write::Retry) => {
                &[Protection::Unasked, Protection::Asks, Protection::Exempt]
            }
            // A Protected Environment's removal is refused before its gate ("unprotect it
            // first", `removal::guard`).
            Command::Store(Write::Remove) | Command::Drain | Command::Machine(_) => {
                &[Protection::Unasked]
            }
            Command::Store(_) => &[Protection::Unasked, Protection::Asks],
        };
        let theirs: &[bool] = match self.command {
            Command::Store(Write::Cancel) => &[true, false],
            Command::Store(_) | Command::Drain | Command::Machine(_) => &[true],
        };
        let signers: &[Signer] = if self.command.machine() {
            &[Signer::Person]
        } else {
            &[Signer::Person, Signer::Agent]
        };
        let mut states = Vec::new();
        for &protection in protections {
            for &theirs in theirs {
                for &signer in signers {
                    states.push(State {
                        facts: Facts {
                            protection,
                            theirs,
                            signer,
                        },
                        live: Plan::Asked,
                        edited: false,
                        trusted: true,
                        crashed: false,
                        global: None,
                        silent: None,
                        unlisted: false,
                        off: false,
                        data: Data::Stays,
                        data_late: false,
                        swapped: None,
                        exec: Exec::Ready,
                        wrote: None,
                        admission: None,
                    });
                }
            }
        }
        states
    }

    fn actions(&self, state: &State, actions: &mut Vec<Action>) {
        if state.over() {
            return;
        }
        if !state.edited {
            actions.extend(self.edits(state).iter().map(|plan| Action::Edit(*plan)));
        }
        self.server_changes(state, actions);
        actions.extend(
            self.data_changes(state)
                .iter()
                .map(|data| Action::Data(*data)),
        );
        if self.command.machine() && state.trusted {
            actions.push(Action::Revoke);
        }
        match state.exec {
            Exec::Ready => actions.push(Action::Send),
            Exec::Resending => actions.push(Action::Resend),
            Exec::Waiting { .. } => {
                actions.push(Action::Run);
                if self.command == Command::Drain && !state.crashed {
                    actions.push(Action::Crash);
                }
            }
            Exec::Covered { .. } => {
                actions.push(Action::Engine);
                if !state.crashed {
                    actions.push(Action::Crash);
                }
            }
            Exec::Off => actions.push(Action::Reread),
            Exec::Moving => actions.push(Action::Volumes),
            Exec::Admitting { read } => {
                actions.push(Action::Admit(true));
                actions.push(Action::Admit(false));
                if state.swapped.is_none() && read != Data::Global {
                    actions.push(Action::Swap);
                }
            }
            Exec::Cutting { read } => {
                actions.push(Action::Cut);
                if state.swapped.is_none() && read != Data::Global {
                    actions.push(Action::Swap);
                }
            }
            Exec::Ran | Exec::Refused { .. } => {}
        }
    }

    fn next_state(&self, last: &State, action: Action) -> Option<State> {
        let mut state = last.clone();
        match action {
            Action::Edit(plan) => {
                state.live = plan;
                state.edited = true;
            }
            Action::Global { chosen } => {
                state.global = Some(Landed {
                    chosen,
                    before: state.window()?,
                });
            }
            Action::Silent => {
                state.silent = Some(Seen {
                    plan: state.live,
                    global: state.global.is_some(),
                });
            }
            Action::Unlisted => state.unlisted = true,
            Action::Revoke => state.trusted = false,
            Action::Send => match self.command {
                Command::Store(write) => self.send_store(&mut state, write),
                Command::Drain => self.send_drain(&mut state),
                Command::Machine(_) if state.trusted => {
                    state.exec = Exec::Waiting {
                        digest: Plan::Asked,
                    };
                }
                Command::Machine(_) => state.refuse(Why::Revoked),
            },
            Action::Resend => {
                let Command::Store(write) = self.command else {
                    return None;
                };
                match self.store_gate(
                    write,
                    state.facts,
                    state.live,
                    Approval::Approved(Plan::Asked),
                ) {
                    Ok(()) => self.run(&mut state),
                    Err(_) => state.refuse(Why::Asked),
                }
            }
            Action::Run => {
                let Exec::Waiting { digest } = state.exec else {
                    return None;
                };
                match self.command {
                    Command::Drain => self.run_drain(&mut state, digest),
                    Command::Machine(Route::Turned)
                        if !state.trusted && !self.broken.turn_trusts_arrival =>
                    {
                        state.refuse(Why::Revoked);
                    }
                    Command::Machine(Route::Installed)
                        if !state.trusted && !self.broken.installed_trusts_arrival =>
                    {
                        state.refuse(Why::Revoked);
                    }
                    Command::Machine(_) | Command::Store(_) => self.run(&mut state),
                }
            }
            Action::Crash => {
                let (Exec::Waiting { digest } | Exec::Covered { digest }) = state.exec else {
                    return None;
                };
                state.exec = Exec::Waiting { digest };
                state.crashed = true;
                // What changed during the dead attempt lands before the retry's compare.
                state.data_late = false;
                if let Some(global) = &mut state.global
                    && global.before == Before::Read
                {
                    global.before = Before::Compare;
                }
            }
            Action::Data(data) => {
                state.data = data;
                state.data_late = state.compared();
            }
            Action::Engine => self.engine_read(&mut state, false),
            Action::Reread => self.engine_read(&mut state, true),
            Action::Volumes => self.drain_volumes(&mut state),
            Action::Admit(admitted) => {
                let Exec::Admitting { read } = state.exec else {
                    return None;
                };
                if !admitted {
                    state.admission = Some(Admission::Stayed);
                    state.exec = Exec::Ran;
                } else if self.holder_refuses(read, state.swapped) {
                    state.admission = Some(Admission::HolderRefused);
                    state.exec = Exec::Ran;
                } else {
                    state.exec = Exec::Cutting { read };
                }
            }
            Action::Swap => {
                state.swapped = Some(match state.exec {
                    Exec::Admitting { .. } => Swap::Admit,
                    Exec::Cutting { .. } => Swap::Cut,
                    Exec::Ready
                    | Exec::Resending
                    | Exec::Waiting { .. }
                    | Exec::Covered { .. }
                    | Exec::Off
                    | Exec::Moving
                    | Exec::Ran
                    | Exec::Refused { .. } => return None,
                });
            }
            Action::Cut => {
                let Exec::Cutting { read } = state.exec else {
                    return None;
                };
                if self.holder_refuses(read, state.swapped) {
                    state.admission = Some(Admission::HolderRefused);
                } else {
                    // The cut stops the Container that holds the Volume now.
                    let holder = if state.swapped.is_some() {
                        Data::Global
                    } else {
                        read
                    };
                    state.admission = Some(Admission::Moved(holder));
                    if holder == Data::Global {
                        state.wrote = Some(Wrote {
                            plan: Plan::Global,
                            valid: self.valid(&state),
                        });
                    }
                }
                state.exec = Exec::Ran;
            }
        }
        Some(state)
    }

    fn properties(&self) -> Vec<Property<Self>> {
        let mut properties = vec![
            Property::always(BOUND, |_, state: &State| {
                state
                    .wrote
                    .is_none_or(|wrote| wrote.plan == Plan::Asked && wrote.valid)
            }),
            Property::sometimes(YES_RUNS, |_, state: &State| {
                state.wrote
                    == Some(Wrote {
                        plan: Plan::Asked,
                        valid: true,
                    })
            }),
        ];
        match self.command {
            Command::Store(write) => {
                properties.push(Property::sometimes(EDIT_ASKS, |_, state: &State| {
                    matches!(
                        state.exec,
                        Exec::Refused {
                            why: Why::Asked,
                            ..
                        }
                    ) && state.live != Plan::Asked
                }));
                properties.push(Property::sometimes(AGENT_REFUSED, |_, state: &State| {
                    matches!(
                        state.exec,
                        Exec::Refused {
                            why: Why::Agent,
                            ..
                        }
                    )
                }));
                if write != Write::Remove {
                    properties.push(Property::sometimes(MEMBER_ASKS, |_, state: &State| {
                        state.facts.protection == Protection::Asks
                            && state.facts.signer == Signer::Person
                            && state.live == Plan::Asked
                            && matches!(
                                state.exec,
                                Exec::Refused {
                                    why: Why::Protected,
                                    ..
                                }
                            )
                    }));
                }
            }
            Command::Drain => {
                properties.push(Property::sometimes(EDIT_ASKS, |_, state: &State| {
                    matches!(
                        state.exec,
                        Exec::Refused {
                            why: Why::Asked,
                            ..
                        }
                    ) && state.live != Plan::Asked
                }));
                properties.push(Property::sometimes(DRAIN_CHANGED, |_, state: &State| {
                    matches!(
                        state.exec,
                        Exec::Refused {
                            why: Why::Changed,
                            ..
                        }
                    )
                }));
                properties.push(Property::sometimes(AGENT_REFUSED, |_, state: &State| {
                    matches!(
                        state.exec,
                        Exec::Refused {
                            why: Why::Agent,
                            ..
                        }
                    )
                }));
                properties.push(Property::sometimes(DRAIN_RETRIED, |_, state: &State| {
                    state.crashed && state.wrote.is_some()
                }));
                properties.push(Property::sometimes(
                    CLOUD_REFUSES_GLOBAL,
                    |_, state: &State| {
                        matches!(
                            state.exec,
                            Exec::Refused {
                                why: Why::Global,
                                ..
                            }
                        )
                    },
                ));
                properties.push(Property::sometimes(
                    ENGINE_REFUSES_UNCHOSEN,
                    |_, state: &State| {
                        state.global.is_some_and(|global| {
                            !global.chosen && matches!(global.before, Before::Read | Before::Reread)
                        }) && matches!(
                            state.exec,
                            Exec::Refused {
                                why: Why::EngineGlobal,
                                ..
                            }
                        )
                    },
                ));
                properties.push(Property::sometimes(UNLISTED_REFUSES, |_, state: &State| {
                    state.global.is_some()
                        && matches!(
                            state.exec,
                            Exec::Refused {
                                why: Why::Unobservable,
                                ..
                            }
                        )
                }));
                properties.push(Property::sometimes(OFF_REFUSED, |_, state: &State| {
                    state.off
                        && matches!(
                            state.exec,
                            Exec::Refused {
                                why: Why::EngineGlobal | Why::Unobservable,
                                ..
                            }
                        )
                }));
                properties.push(Property::sometimes(
                    UNANSWERED_REFUSES,
                    |_, state: &State| {
                        state.silent
                            == Some(Seen {
                                plan: Plan::Asked,
                                global: false,
                            })
                            && state.live != Plan::Asked
                            && matches!(
                                state.exec,
                                Exec::Refused {
                                    why: Why::Unseen,
                                    ..
                                }
                            )
                    },
                ));
                properties.push(Property::sometimes(
                    GLOBAL_KEEPS_VOLUME,
                    |_, state: &State| {
                        state.data == Data::Global
                            && state.exec == Exec::Ran
                            && state.admission.is_none()
                    },
                ));
                properties.push(Property::sometimes(SWITCHED_MOVES, |_, state: &State| {
                    state.data_late && state.admission == Some(Admission::Moved(Data::Switched))
                }));
                properties.push(Property::sometimes(
                    ADMISSION_REFUSES,
                    |_, state: &State| state.admission == Some(Admission::Stayed),
                ));
                properties.push(Property::sometimes(HOLDER_REFUSES, |_, state: &State| {
                    state.swapped == Some(Swap::Cut)
                        && state.admission == Some(Admission::HolderRefused)
                }));
            }
            Command::Machine(_) => {
                properties.push(Property::sometimes(
                    REVOKED_UNDER_TURN,
                    |_, state: &State| {
                        state.exec
                            == Exec::Refused {
                                why: Why::Revoked,
                                waited: true,
                            }
                    },
                ));
            }
        }
        properties
    }
}

const BOUND: &str = "every effect runs the plan a valid yes was given for";
const YES_RUNS: &str = "a yes runs the plan it was given for";
const EDIT_ASKS: &str = "a plan changed since the ask asks again";
const AGENT_REFUSED: &str = "an agent's yes is refused";
const MEMBER_ASKS: &str = "a member's own yes reaches the Store and asks an owner or admin";
const DRAIN_CHANGED: &str = "a Drain whose plan changed after its gate is refused when it runs";
const DRAIN_RETRIED: &str = "a Drain runs after Inngest ran its step again";
const REVOKED_UNDER_TURN: &str = "a key revoked while its call waited is refused under the turn";
const CLOUD_REFUSES_GLOBAL: &str =
    "a user Global Cloud reads on the drained Server refuses the Drain before the Engine";
const ENGINE_REFUSES_UNCHOSEN: &str = "a user Global the Drain's scope left out, landed after \
     Cloud's compare, refuses the Drain at the Engine";
const UNLISTED_REFUSES: &str =
    "a Server the Engine can't list refuses the Drain, though a Global runs there";
const OFF_REFUSED: &str = "a Global or a lost listing at the Engine's second read refuses the \
     Drain with services off";
const UNANSWERED_REFUSES: &str = "a Server that stopped answering Cloud refuses the Drain, \
     though its last rows match the yes";
const GLOBAL_KEEPS_VOLUME: &str =
    "a Service made a Global after the Engine left it keeps its Volume";
const SWITCHED_MOVES: &str = "a Volume the Service switched to after Cloud's compare and \
     before services-off moves once admitted, which the carried yes never named";
const ADMISSION_REFUSES: &str = "admission refuses a planned Volume move, and the Volume stays";
const HOLDER_REFUSES: &str = "a Global that replaced the Volume's holder after admission is \
     refused at the Move's step, and the Volume stays";

/// `path` under `model` in the code's words: each step, and what stands once it ran.
fn story(model: &ApprovalBinding, path: &[(State, Option<Action>)]) -> String {
    let mut story = String::new();
    let mut led_by = None;
    for (n, (state, action)) in path.iter().enumerate() {
        let what = match led_by {
            None if model.command.machine() => format!(
                "start: {:?}, a Device's call, its key accepted by the Machine",
                model.command
            ),
            None => format!(
                "start: {:?}, {} carries a yes to the plan it was asked about; protection {:?}{}",
                model.command,
                match state.facts.signer {
                    Signer::Person => "a person",
                    Signer::Agent => "an agent",
                },
                state.facts.protection,
                if state.facts.theirs {
                    ""
                } else {
                    ", the Deployment is the caller's own"
                },
            ),
            Some(action) => reads_as(model, action, state),
        };
        led_by = *action;
        let wrote = state.wrote.map_or_else(
            || "nothing".to_owned(),
            |wrote| {
                format!(
                    "{:?} under a yes that {}",
                    wrote.plan,
                    if wrote.valid {
                        "counts"
                    } else {
                        "doesn't count"
                    }
                )
            },
        );
        let volume = if model.command == Command::Drain {
            format!(
                " server=(global {:?}, Cloud reads {}, Engine lists it {}, services {}) data={:?} swapped={:?} admission={:?}",
                state.global,
                state.silent.map_or_else(
                    || "it now".to_owned(),
                    |seen| format!("its last rows {seen:?}")
                ),
                !state.unlisted,
                if state.off { "off" } else { "on" },
                state.data,
                state.swapped,
                state.admission
            )
        } else {
            String::new()
        };
        let _ = writeln!(
            story,
            "{n:2}. {what}\n    plan={:?} key={} exec={:?} wrote={wrote}{volume}",
            state.live,
            if state.trusted { "accepted" } else { "revoked" },
            state.exec,
        );
    }
    story
}

/// The step `action` took, reaching `after`.
fn reads_as(model: &ApprovalBinding, action: Action, after: &State) -> String {
    let broken = model.broken;
    let outcome = match after.exec {
        Exec::Ran => "and it runs".to_owned(),
        Exec::Refused { why, .. } => format!("and it is refused: {why:?}"),
        Exec::Resending => "and Cloud's first call asks for the yes's digest".to_owned(),
        Exec::Waiting { digest } => format!("and it waits, holding digest {digest:?}"),
        Exec::Covered { digest } => format!("and it matches digest {digest:?}"),
        Exec::Off => "and services go off, the second read to come".to_owned(),
        Exec::Moving => "and the Engine answers".to_owned(),
        Exec::Admitting { read } => format!("and it plans a move from what it read: {read:?}"),
        Exec::Cutting { .. } => "and the Move starts, its cut to come".to_owned(),
        Exec::Ready => String::new(),
    };
    match action {
        Action::Edit(plan) => format!("editor: {}", edit_words(plan)),
        Action::Global { chosen: true } => {
            "server: a Service the Drain's scope chose is redeployed there as a Global".to_owned()
        }
        Action::Global { chosen: false } => {
            "server: a Full caller starts a user Global there, outside the Drain's scope".to_owned()
        }
        Action::Silent => {
            "server: it stops answering Cloud's probes, so the frame lists it unanswered with its last rows"
                .to_owned()
        }
        Action::Unlisted => "server: it stops answering the Engine's listing".to_owned(),
        Action::Revoke => {
            "remover: set_device removes the Device and its slot under its own exclusive turn"
                .to_owned()
        }
        Action::Crash => {
            "Inngest: executeDrainOnce's step dies before the Engine is asked, to run again"
                .to_owned()
        }
        Action::Data(data) => format!("frame: {}", data_words(data)),
        Action::Engine => format!(
            "Engine: drainMachine's preflight lists the drained Server and chosen_on {}, {outcome}",
            engine_words(broken)
        ),
        Action::Reread => format!(
            "Engine: preflight lists the drained Server again with services off, and chosen_on {}, {outcome}",
            engine_words(broken)
        ),
        Action::Volumes if broken.volume_moves_global => format!(
            "Cloud: planDrainVolumes reads a new frame through dataCheck, which takes a Global's copy as the holder (under Broken), {outcome}"
        ),
        Action::Volumes => {
            format!("Cloud: planDrainVolumes reads a new frame through dataCheck, {outcome}")
        }
        Action::Admit(true) => format!(
            "Cloud: admitDrainVolume reads the holder through {}, {outcome}",
            holder_words(broken)
        ),
        Action::Admit(false) => {
            "Cloud: admitDrainVolume refuses the move, and the Volume stays".to_owned()
        }
        Action::Swap => {
            "server: a Full caller replaces the Service holding the Volume with a Global".to_owned()
        }
        Action::Cut => format!(
            "Cloud: the Move's step reads the holder through {}, then cuts it: {:?}",
            holder_words(broken),
            after.admission
        ),
        Action::Send => match model.command {
            Command::Store(write) if broken.cloud_calls_twice => format!(
                "executor: handleConfigRequest's first Store call carries no yes (under Broken), {} weighs `required`, {outcome}",
                gate_words(write)
            ),
            Command::Store(write) => format!(
                "executor: handleConfigRequest sends the yes into the write, {} weighs it there{}, {outcome}",
                gate_words(write),
                if broken == Broken::default() {
                    ""
                } else {
                    " (under Broken)"
                }
            ),
            Command::Drain => format!(
                "executor: drainServer plans, drainRefusal{} and gateOperation weighs the yes, {outcome}",
                cloud_words(broken)
            ),
            Command::Machine(Route::Turned) => {
                format!("executor: the call arrives and waits for its turn, {outcome}")
            }
            Command::Machine(Route::Installed) => format!(
                "executor: finish_installed's admit_installation checks the caller's access and the work drains, {outcome}"
            ),
        },
        Action::Resend => match model.command {
            Command::Store(write) => format!(
                "executor: handleConfigRequest's second Store call carries the yes (under Broken), {} weighs it, {outcome}",
                gate_words(write)
            ),
            Command::Drain | Command::Machine(_) => outcome,
        },
        Action::Run => match model.command {
            Command::Drain => format!(
                "Inngest: executeDrainOnce plans again, drainRefusal{} and stillCovered compares the row, {outcome}",
                cloud_words(broken)
            ),
            Command::Machine(Route::Turned) if broken.turn_trusts_arrival => format!(
                "Machine: finish_admitted takes the turn and runs what arrival admitted, unchecked (under Broken), {outcome}"
            ),
            Command::Machine(Route::Turned) => format!(
                "Machine: finish_admitted takes the turn, then admit_installation checks the caller's access under it, {outcome}"
            ),
            Command::Machine(Route::Installed) if broken.installed_trusts_arrival => format!(
                "Machine: the work takes exclusive_turn and removes the Container, unchecked (under Broken), {outcome}"
            ),
            Command::Machine(Route::Installed) => format!(
                "Machine: the work takes exclusive_turn, which checks the caller's access under it, {outcome}"
            ),
            Command::Store(_) => outcome,
        },
    }
}

const fn edit_words(plan: Plan) -> &'static str {
    match plan {
        Plan::Asked => "the plan goes back to the one asked about",
        Plan::Other => "the plan moves on: it now destroys something else, at a new version",
        Plan::Harmless => "the plan changes to one that destroys nothing",
        Plan::Discarded => "the draft is discarded: nothing is staged, Working State is Head",
        Plan::Forgets => {
            "every Server leaves: forgettable answers Forgotten, so the Store only forgets what ran"
        }
        Plan::Gone => "the drained Server leaves the frame: drainPlan answers null",
        Plan::Started => {
            "the runner claims the queued Deployment: it is running now, at the same review version"
        }
        Plan::Global => "a user Global runs on the drained Server",
    }
}

/// What `chosen_on` refuses, as `broken` leaves it.
const fn engine_words(broken: Broken) -> &'static str {
    match (
        broken.engine_skips_globals,
        broken.engine_global_scoped,
        broken.engine_reads_unlisted_empty,
    ) {
        (true, _, _) => "drains past any Global (under Broken)",
        (false, true, _) => "looks for a Global only among the chosen Services (under Broken)",
        (false, false, true) => {
            "reads a Server that didn't answer as running nothing (under Broken)"
        }
        (false, false, false) => "refuses a Server it can't list or that runs a user Global",
    }
}

/// What the holder check refuses, as `broken` leaves it.
const fn holder_words(broken: Broken) -> &'static str {
    match (broken.move_skips_holder, broken.volume_moves_global) {
        (true, _) => "no check (under Broken)",
        (false, true) => "a check that takes a Global's copy (under Broken)",
        (false, false) => {
            "holderOn and the Drain's pin, which refuse a Global or another Container"
        }
    }
}

/// What `drainRefusal` reads, as `broken` leaves it.
const fn cloud_words(broken: Broken) -> &'static str {
    if broken.cloud_reads_unanswered {
        " reads an unanswered Server's last rows (under Broken)"
    } else {
        " refuses an unanswered Server or a user Global"
    }
}

const fn data_words(data: Data) -> &'static str {
    match data {
        Data::Stays => "the Service's Volume stays put",
        Data::Frees => {
            "the Volume's reason to stay clears: its sharer stopped or its Container started"
        }
        Data::Switched => {
            "a Deploy switches the Service from its shared Volume V1 to V2, onto the drained Server"
        }
        Data::Global => {
            "a Full caller replaces the Service with a Global, whose copy now holds the Volume"
        }
    }
}

const fn gate_words(write: Write) -> &'static str {
    match write {
        Write::Save => "publish and review::approve",
        Write::Deploy => "review::approve (Shipping::Deploy)",
        Write::Retry => "approve_retry",
        Write::RemovalRetry => "admit's removal arm, ask_taking_off and approve_shut_down",
        Write::ShutDown => "shut_down, ask_taking_off and approve_shut_down",
        Write::Remove => "removal::gate",
        Write::Cancel => "removal::cancel and approve_cancel",
        Write::Move => "move_asks and approve_move",
    }
}

#[cfg(test)]
mod tests {
    use stateright::Checker;

    use super::*;
    use crate::harness::{Narrated, Path, breaks_where, check, first_break, last};

    impl Narrated for ApprovalBinding {
        fn story(&self, path: &[(State, Option<Action>)]) -> String {
            story(self, path)
        }
    }

    fn code(command: Command) -> ApprovalBinding {
        without(command, Broken::default())
    }

    const fn without(command: Command, broken: Broken) -> ApprovalBinding {
        ApprovalBinding {
            command,
            broken,
            open_paths: false,
        }
    }

    /// The code with the changes its digest doesn't bind.
    fn open(command: Command) -> ApprovalBinding {
        ApprovalBinding {
            open_paths: true,
            ..code(command)
        }
    }

    /// The first path in BFS order that breaks the property under `model` and ends in a
    /// state `ends` accepts, printed as a story.
    fn breaks_ending(
        model: ApprovalBinding,
        ends: impl Fn(&State) -> bool + Send + Sync + 'static,
    ) -> Path<ApprovalBinding> {
        breaks_where(model, BOUND, move |path| ends(&last(path)))
    }

    /// The first path on which a person's yes, its protection asking nothing of them,
    /// lets `plan` run under `model`.
    fn person_runs(model: ApprovalBinding, protection: Protection, plan: Plan) -> State {
        last(&breaks_ending(model, move |state| {
            state.facts.signer == Signer::Person
                && state.facts.protection == protection
                && state.wrote == Some(Wrote { plan, valid: true })
        }))
    }

    /// Every gate as it ships binds a yes to its plan and its moment, and reaches every
    /// branch the code handles: a yes runs its plan, an edited plan asks again, an agent's
    /// yes and a member's in a Protected Environment are refused, a Drain refuses a plan
    /// that changed after its gate and runs after a retry, and a turn refuses a key revoked
    /// while its call waited. The changes `open_paths` adds are the open paths at the end of
    /// this module.
    #[test]
    fn the_code_binds_every_yes_to_its_plan_and_moment() {
        for command in Command::ALL {
            let checker = check(code(command));
            println!("{command:?}: {} states", checker.unique_state_count());
            checker.assert_properties();
        }
    }

    /// f06d2a6351, Cloud's half. Cloud called the Store with no yes and resent the yes only
    /// after an ask that named its digest. A plan edited to destroy nothing never asked, so
    /// the first call wrote it, and the yes the request carried was never weighed.
    #[test]
    fn a_save_needs_its_yes_in_the_write() {
        for write in [Write::Save, Write::Deploy] {
            let broken = Broken {
                cloud_calls_twice: true,
                ..Broken::default()
            };
            let ran = person_runs(
                without(Command::Store(write), broken),
                Protection::Unasked,
                Plan::Harmless,
            );
            assert_eq!(ran.exec, Exec::Ran, "{write:?}");
        }
    }

    /// f06d2a6351, the Store's half: with the yes in the write, `approve`'s shortcut for a
    /// plan that destroys nothing still let it through unweighed.
    #[test]
    fn approve_weighs_a_carried_yes_before_its_shortcut() {
        for write in [Write::Save, Write::Deploy] {
            let broken = Broken {
                approve_skips_yes: true,
                ..Broken::default()
            };
            person_runs(
                without(Command::Store(write), broken),
                Protection::Unasked,
                Plan::Harmless,
            );
        }
    }

    /// 0f29e0d3f3: a Save with nothing staged returned before any gate, so a yes to a draft
    /// someone discarded answered success.
    #[test]
    fn an_empty_save_weighs_its_yes() {
        let broken = Broken {
            empty_save_skips_yes: true,
            ..Broken::default()
        };
        person_runs(
            without(Command::Store(Write::Save), broken),
            Protection::Unasked,
            Plan::Discarded,
        );
    }

    /// 0f29e0d3f3: once 8836d08f2f weighed a carried yes when every Server left, the digest
    /// still bound no `forgets`, so a yes to taking Services off forgot them.
    #[test]
    fn a_shutdown_binds_whether_it_only_forgets() {
        for write in [Write::ShutDown, Write::RemovalRetry] {
            let broken = Broken {
                forget_unbound: true,
                ..Broken::default()
            };
            person_runs(
                without(Command::Store(write), broken),
                Protection::Unasked,
                Plan::Forgets,
            );
        }
    }

    /// 0f29e0d3f3: a cancel weighed nothing unless it stopped someone else's Deployment, so
    /// a yes to cancelling one's own ran after the Environment moved on.
    #[test]
    fn a_cancel_weighs_its_yes_whoever_admitted_the_deployment() {
        let broken = Broken {
            cancel_skips_unless_theirs: true,
            ..Broken::default()
        };
        let ran = person_runs(
            without(Command::Store(Write::Cancel), broken),
            Protection::Unasked,
            Plan::Other,
        );
        assert!(!ran.facts.theirs, "the caller's own Deployment");
    }

    /// 8836d08f2f: each shortcut that let a write through unasked ran before the digest
    /// comparison. Each flag restores one, and each breaks the property alone.
    #[test]
    fn every_shortcut_weighs_a_carried_yes() {
        let cases: [(Write, Broken, Plan); 6] = [
            (
                Write::Retry,
                Broken {
                    retry_skips_yes: true,
                    ..Broken::default()
                },
                Plan::Harmless,
            ),
            (
                Write::ShutDown,
                Broken {
                    shutdown_skips_yes: true,
                    ..Broken::default()
                },
                Plan::Harmless,
            ),
            (
                Write::ShutDown,
                Broken {
                    forget_skips_yes: true,
                    ..Broken::default()
                },
                Plan::Forgets,
            ),
            (
                Write::RemovalRetry,
                Broken {
                    forget_skips_yes: true,
                    ..Broken::default()
                },
                Plan::Forgets,
            ),
            (
                Write::Remove,
                Broken {
                    remove_skips_yes: true,
                    ..Broken::default()
                },
                Plan::Harmless,
            ),
            (
                Write::Move,
                Broken {
                    move_skips_yes: true,
                    ..Broken::default()
                },
                Plan::Other,
            ),
        ];
        for (write, broken, plan) in cases {
            person_runs(
                without(Command::Store(write), broken),
                Protection::Unasked,
                plan,
            );
        }
    }

    /// 8836d08f2f: a protector's retry that destroys nothing passed as its plain Deploy
    /// does, yes or not. Only a protector's token reaches this shortcut: with it alone
    /// restored, no other protection breaks the property.
    #[test]
    fn a_protector_s_retry_weighs_its_yes() {
        let broken = Broken {
            protector_retry_skips_yes: true,
            ..Broken::default()
        };
        let model = without(Command::Store(Write::Retry), broken);
        person_runs(model, Protection::Exempt, Plan::Harmless);
        let other = first_break(
            model,
            BOUND,
            |path| last(path).facts.protection != Protection::Exempt,
            false,
        );
        assert!(other.is_none(), "only a protector's retry breaks it");
    }

    /// 8836d08f2f: `approve_cancel` returned outside a Protected Environment, so a yes to
    /// cancelling someone else's Deployment ran after the Environment moved on.
    #[test]
    fn a_cancel_weighs_its_yes_outside_a_protected_environment() {
        let broken = Broken {
            cancel_skips_unprotected: true,
            ..Broken::default()
        };
        breaks_ending(without(Command::Store(Write::Cancel), broken), |state| {
            state.facts.signer == Signer::Person
                && state.facts.theirs
                && state.wrote
                    == Some(Wrote {
                        plan: Plan::Other,
                        valid: true,
                    })
        });
    }

    /// 1c93248b9b: the Config API took an agent's yes. With the yes in the write, the Store
    /// runs an agent's echo of the digest an ask showed it.
    #[test]
    fn the_config_api_refuses_an_agent_s_yes() {
        let broken = Broken {
            agent_yes_unrefused: true,
            ..Broken::default()
        };
        breaks_ending(without(Command::Store(Write::Save), broken), |state| {
            state.facts.signer == Signer::Agent
                && state.wrote
                    == Some(Wrote {
                        plan: Plan::Asked,
                        valid: false,
                    })
        });
    }

    /// 1c93248b9b as the handler stood then: two calls and no agent check, so an agent's
    /// resend of the asked digest went through on the second call.
    #[test]
    fn the_two_call_handler_ran_an_agent_s_yes() {
        let broken = Broken {
            cloud_calls_twice: true,
            agent_yes_unrefused: true,
            ..Broken::default()
        };
        let path = breaks_ending(without(Command::Store(Write::Save), broken), |state| {
            state.facts.signer == Signer::Agent
                && state.wrote.is_some_and(|wrote| wrote.plan == Plan::Asked)
        });
        assert!(
            path.iter()
                .any(|(_, action)| *action == Some(Action::Resend))
        );
    }

    /// 1c93248b9b: `gateOperation` let a Drain with no effects through before comparing a
    /// carried digest, and recorded that plan's digest, so the fixed `stillCovered` then
    /// matched it.
    #[test]
    fn a_drain_s_gate_weighs_its_yes_before_its_shortcut() {
        let broken = Broken {
            gate_skips_yes: true,
            ..Broken::default()
        };
        person_runs(
            without(Command::Drain, broken),
            Protection::Unasked,
            Plan::Harmless,
        );
    }

    /// 1c93248b9b: `stillCovered` covered any plan with no effects, so a Drain a person
    /// said yes to ran once its plan changed to destroy nothing after its gate.
    #[test]
    fn a_drain_runs_only_its_exact_plan() {
        let broken = Broken {
            drain_runs_harmless: true,
            ..Broken::default()
        };
        person_runs(
            without(Command::Drain, broken),
            Protection::Unasked,
            Plan::Harmless,
        );
    }

    /// bad55e1aed: `stillCovered` still covered a Drain whose Server left the frame.
    #[test]
    fn a_drain_runs_only_on_a_present_plan() {
        let broken = Broken {
            drain_runs_gone: true,
            ..Broken::default()
        };
        person_runs(
            without(Command::Drain, broken),
            Protection::Unasked,
            Plan::Gone,
        );
    }

    /// 273b77fabc: a Server checked an Operator's teardown when the request arrived and ran
    /// it whenever its turn came. Its statements are gone (cfd221baf1), so this replays the
    /// shape on what authorizes the call now, the caller's management access, on the route
    /// that checks it under the turn.
    #[test]
    fn a_turn_checks_the_caller_again() {
        let broken = Broken {
            turn_trusts_arrival: true,
            ..Broken::default()
        };
        let ran = last(&breaks_ending(
            without(Command::Machine(Route::Turned), broken),
            |state| state.wrote.is_some(),
        ));
        assert!(!ran.trusted, "the key was revoked before the turn");
    }

    /// A guard, not a past bug: once the yes went into the Store's write (f06d2a6351), the
    /// Store had to refuse a member's own CLI yes to what a Protected Environment asks of
    /// an owner or admin, which Cloud's `isProtectedAsk` refused before the second call.
    #[test]
    fn a_member_s_yes_never_answers_a_protected_ask() {
        let broken = Broken {
            member_yes_answers_protected: true,
            ..Broken::default()
        };
        breaks_ending(without(Command::Store(Write::Save), broken), |state| {
            state.facts.protection == Protection::Asks
                && state.wrote
                    == Some(Wrote {
                        plan: Plan::Asked,
                        valid: false,
                    })
        });
    }

    /// #4558: `finish_installed` checked the caller's management access when the call
    /// arrived; its work drained, then took `exclusive_turn`, which checked nothing, and
    /// removed the Container. A Device removed while it drained still had its Container
    /// removed.
    #[test]
    fn an_installed_step_checks_the_caller_again_under_its_turn() {
        let broken = Broken {
            installed_trusts_arrival: true,
            ..Broken::default()
        };
        let ran = last(&breaks_ending(
            without(Command::Machine(Route::Installed), broken),
            |state| state.wrote.is_some(),
        ));
        assert!(!ran.trusted, "the key was revoked while the call drained");
    }

    /// Open, #4555. A Cancel's digest binds the Deployment and the review version. The
    /// runner's claim moves neither, so a yes given to take a queued Deployment out of the
    /// queue stops it partway once it runs, the caller's own or someone else's.
    #[test]
    fn a_cancel_s_yes_while_queued_stops_a_started_deployment() {
        for theirs in [true, false] {
            breaks_ending(open(Command::Store(Write::Cancel)), move |state| {
                state.facts.theirs == theirs
                    && state.wrote
                        == Some(Wrote {
                            plan: Plan::Started,
                            valid: true,
                        })
            });
        }
    }

    /// The Drain under `broken` reaches an effect a user Global sat beside, on a path
    /// `ends` accepts.
    fn drains_beside_a_global(
        broken: Broken,
        ends: impl Fn(&State) -> bool + Send + Sync + 'static,
    ) -> State {
        last(&breaks_ending(
            without(Command::Drain, broken),
            move |state| {
                state.wrote
                    == Some(Wrote {
                        plan: Plan::Global,
                        valid: true,
                    })
                    && ends(state)
            },
        ))
    }

    /// #4567, the fix itself. As `main` did before it, the Engine drains a Server running a
    /// user Global, so one that landed after Cloud's compare loses the copy there, which
    /// may be its last that works while the others crash-loop.
    #[test]
    fn the_engine_refuses_a_drain_beside_a_user_global() {
        let broken = Broken {
            engine_skips_globals: true,
            ..Broken::default()
        };
        drains_beside_a_global(broken, |state| {
            state.global.is_some_and(|global| {
                global.chosen && matches!(global.before, Before::Read | Before::Reread)
            })
        });
    }

    /// #4567 before its fourth round: `chosen_on` looked for a Global only in the scope Cloud
    /// saved at the request, so a Global that landed after Cloud's compare, never in that
    /// scope, was drained beside. A Global the scope chose was still refused.
    #[test]
    fn the_engine_refuses_a_global_its_scope_left_out() {
        let broken = Broken {
            engine_global_scoped: true,
            ..Broken::default()
        };
        let model = without(Command::Drain, broken);
        drains_beside_a_global(broken, |state| {
            state.global.is_some_and(|global| !global.chosen)
        });
        let chosen = first_break(
            model,
            BOUND,
            |path| last(path).global.is_some_and(|global| global.chosen),
            false,
        );
        assert!(
            chosen.is_none(),
            "a Global the scope chose is still refused"
        );
    }

    /// A guard, not a past bug: `chosen_on` refuses a Server whose listing didn't answer.
    /// Read as running nothing, a Server that runs a Global unseen is drained.
    #[test]
    fn the_engine_refuses_a_server_it_cannot_list() {
        let broken = Broken {
            engine_reads_unlisted_empty: true,
            ..Broken::default()
        };
        drains_beside_a_global(broken, |state| state.unlisted);
    }

    /// #4567 before its fourth round: `drainPlan` read a Server the frame lists
    /// `unanswered` from the rows it last reported, so `stillCovered` matched the yes's
    /// digest while the plan the Engine then ran had moved on.
    #[test]
    fn cloud_refuses_a_server_that_stopped_answering() {
        let broken = Broken {
            cloud_reads_unanswered: true,
            ..Broken::default()
        };
        let ran = last(&breaks_ending(without(Command::Drain, broken), |state| {
            state.silent.is_some()
                && state
                    .wrote
                    .is_some_and(|wrote| wrote.plan != Plan::Asked && wrote.valid)
        }));
        assert_eq!(
            ran.silent.map(|seen| seen.plan),
            Some(Plan::Asked),
            "its last rows matched the yes"
        );
    }

    /// #4567 before its fourth round: `dataCheck` chose, and `holderOn` checked, a Volume's
    /// holder by Container alone, so a Global that replaced the Service after the Engine's
    /// report was the Container its move cut.
    #[test]
    fn a_drain_s_volume_move_never_cuts_a_global() {
        let broken = Broken {
            volume_moves_global: true,
            ..Broken::default()
        };
        let ran = drains_beside_a_global(broken, |state| state.global.is_none());
        assert_eq!(ran.admission, Some(Admission::Moved(Data::Global)));
    }

    /// A guard, not a past bug: the holder check at each Move step. `dataCheck` and
    /// admission read the holder before a Full caller replaces it with a Global, so only the
    /// Move's own read stands between that Global's copy and the cut.
    #[test]
    fn a_move_step_never_cuts_a_global_that_replaced_its_holder() {
        let broken = Broken {
            move_skips_holder: true,
            ..Broken::default()
        };
        let ran = drains_beside_a_global(broken, |state| state.swapped == Some(Swap::Cut));
        assert_ne!(ran.data, Data::Global, "dataCheck read no Global");
        assert_eq!(ran.admission, Some(Admission::Moved(Data::Global)));
    }
}
