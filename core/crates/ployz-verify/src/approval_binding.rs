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
//!   (`Command::edits`).
//! - **The remover** revokes the caller's management access on a Machine once
//!   (`Action::Revoke`): a Full caller's `set_device` removes the Device and its slot under
//!   its own exclusive turn (`machine/local_machine/management_client.rs`).
//! - **The executor** sends the request (`Action::Send`), and where the code splits it, takes
//!   its later steps: Cloud's second Store call before f06d2a6351 (`Action::Resend`), a
//!   Drain's run and Inngest's retry of it (`Action::Run`, `Action::Crash`), a Machine's
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
//!   `executeDrainOnce` runs (`Command::Drain`).
//! - A Machine checks the caller's management access, `require_management_access`
//!   (`ployzd/src/machine/local_machine.rs`), in `admit_installation`
//!   (`ployzd/src/machine/local_machine/turns.rs`). `finish_mutation` and `finish_shared`
//!   take their turn first and check under it; `finish_installed` checks when the call
//!   arrives and its work takes `exclusive_turn` later, which checks nothing
//!   (`Command::Machine`).
//!
//! Out of scope: a yes the dashboard gives itself (a dashboard confirm), a Deploy that waits
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
    /// `Shipping::Publish`. Nothing staged returns early (`publish.rs:319-339`).
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
    /// arrives, and the work takes `exclusive_turn` itself after its drain, which checks
    /// nothing: `ContainerStep::StopThenRemove` and `ContainerStep::Remove`
    /// (`machine/local_machine/container.rs:1491-1543`), `stop_checked`'s stop, and a
    /// Volume cut (`container.rs:674`).
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

    /// The plans the editor may move this command's plan to.
    fn edits(self) -> &'static [Plan] {
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
}

impl Plan {
    /// Whether the plan names a destructive effect.
    const fn destroys(self) -> bool {
        match self {
            Self::Asked | Self::Other | Self::Forgets => true,
            Self::Harmless | Self::Discarded | Self::Gone => false,
        }
    }

    /// The digest the code binds for this plan, written as the plan it names.
    /// `approve_shut_down` binds `forgets`; without it, forgetting reads as taking the
    /// same Services off.
    const fn digest(self, broken: Broken) -> Self {
        match self {
            Self::Forgets if broken.forget_unbound => Self::Asked,
            Self::Asked
            | Self::Other
            | Self::Harmless
            | Self::Discarded
            | Self::Forgets
            | Self::Gone => self,
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
    exec: Exec,
    wrote: Option<Wrote>,
}

impl State {
    const fn over(&self) -> bool {
        matches!(self.exec, Exec::Ran | Exec::Refused { .. })
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
    /// A guard f06d2a6351 needed, not a past bug: `Gate::ask` takes a member's own CLI yes
    /// for what a Protected Environment asks of an owner or admin.
    member_yes_answers_protected: bool,
}

#[derive(Clone, Copy, Debug)]
struct ApprovalBinding {
    command: Command,
    broken: Broken,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Action {
    /// The editor moves the plan.
    Edit(Plan),
    /// The remover revokes the caller's management access.
    Revoke,
    /// The executor sends its request.
    Send,
    /// Cloud's second Store call, with the yes (before f06d2a6351).
    Resend,
    /// The Drain's `executeDrainOnce`, or the Machine's admitted turn.
    Run,
    /// The Drain's step dies before its effect; Inngest runs it again.
    Crash,
}

impl ApprovalBinding {
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

    /// Whether the yes counts now, for an effect about to run.
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

    /// `drainServer`'s `planDrain` and `gateOperation`, then the row it records.
    fn send_drain(&self, state: &mut State) {
        let plan = state.live;
        if plan == Plan::Gone {
            state.refuse(Why::NoPlan);
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

    /// `executeDrainOnce`'s `stillCovered`, for a Drain a person said yes to.
    fn run_drain(&self, state: &mut State, digest: Plan) {
        let plan = state.live;
        let present = plan != Plan::Gone;
        let harmless = !plan.destroys();
        let matches = present && plan.digest(self.broken) == digest;
        let covered = if self.broken.drain_runs_harmless {
            !present || harmless || matches
        } else if self.broken.drain_runs_gone {
            // `plan === null || (plan.effects.length === 0 && !admitted.approved)`.
            !present || matches
        } else {
            matches
        };
        if covered {
            self.run(state);
        } else {
            state.refuse(Why::Changed);
        }
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
                        exec: Exec::Ready,
                        wrote: None,
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
            actions.extend(self.command.edits().iter().map(|plan| Action::Edit(*plan)));
        }
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
                    Command::Machine(_) | Command::Store(_) => self.run(&mut state),
                }
            }
            Action::Crash => state.crashed = true,
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
                            && state.live == Plan::Asked
                            && matches!(state.exec, Exec::Refused { .. })
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
            }
            Command::Machine(Route::Turned) => {
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
            Command::Machine(Route::Installed) => {}
        }
        properties
    }
}

const BOUND: &str = "every effect runs the plan a valid yes was given for";
const YES_RUNS: &str = "a yes runs the plan it was given for";
const EDIT_ASKS: &str = "a plan changed since the ask asks again";
const AGENT_REFUSED: &str = "an agent's yes is refused";
const MEMBER_ASKS: &str = "a member's own yes asks again in a Protected Environment";
const DRAIN_CHANGED: &str = "a Drain whose plan changed after its gate is refused when it runs";
const DRAIN_RETRIED: &str = "a Drain runs after Inngest ran its step again";
const REVOKED_UNDER_TURN: &str = "a key revoked while its call waited is refused under the turn";

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
        let _ = writeln!(
            story,
            "{n:2}. {what}\n    plan={:?} key={} exec={:?} wrote={wrote}",
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
        Exec::Ready => String::new(),
    };
    match action {
        Action::Edit(plan) => format!("editor: {}", edit_words(plan)),
        Action::Revoke => {
            "remover: set_device removes the Device and its slot under its own exclusive turn"
                .to_owned()
        }
        Action::Crash => {
            "Inngest: executeDrainOnce's step dies before its effect, to run again".to_owned()
        }
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
            Command::Drain => {
                format!("executor: drainServer plans and gateOperation weighs the yes, {outcome}")
            }
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
                "Inngest: executeDrainOnce plans again and stillCovered weighs the row, {outcome}"
            ),
            Command::Machine(Route::Turned) if broken.turn_trusts_arrival => format!(
                "Machine: finish_admitted takes the turn and runs what arrival admitted, unchecked (under Broken), {outcome}"
            ),
            Command::Machine(Route::Turned) => format!(
                "Machine: finish_admitted takes the turn, then admit_installation checks the caller's access under it, {outcome}"
            ),
            Command::Machine(Route::Installed) => format!(
                "Machine: the work takes exclusive_turn and removes the Container, checking nothing, {outcome}"
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
        ApprovalBinding { command, broken }
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
    /// while its call waited. `Route::Installed` is the finding below, so it is left out.
    #[test]
    fn the_code_binds_every_yes_to_its_plan_and_moment() {
        for command in Command::ALL {
            if command == Command::Machine(Route::Installed) {
                continue;
            }
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

    /// The finding, still open. `finish_installed` checks the caller's management access
    /// when the call arrives; its work drains, then takes `exclusive_turn`, which checks
    /// nothing, and removes the Container. A Device removed while it drained still has its
    /// Container removed.
    #[test]
    fn an_installed_step_runs_for_a_key_revoked_while_it_drained() {
        let ran = last(&breaks_ending(
            code(Command::Machine(Route::Installed)),
            |state| state.wrote.is_some(),
        ));
        assert!(!ran.trusted, "the key was revoked while the call drained");
    }
}
