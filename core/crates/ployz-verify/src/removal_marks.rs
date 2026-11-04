//! The removal mark's lifecycle on the Config Store: one Environment that `remove()`
//! marked `removing`, its removal Deployment ending on the Servers, the sweep that
//! finishes the mark and purges the Environment, the cancel that leaves it a
//! tombstone, and a pull-request close, interleaved one statement at a time as
//! Postgres runs them at READ COMMITTED.
//!
//! The property: a cancel that returned success keeps its Environment. Once
//! `cancel_removal` commits, the mark never reads `removed` and the Environment row
//! is never purged. Nothing in the model marks it again, so no later `remove()` can
//! excuse either.
//!
//! Each actor is one write transaction (`Storage::write`, storage/mod.rs), and each
//! of its steps is one statement, or one row lock, of the function it copies:
//!
//! - the sweep: `removal::sweep` (removal/due.rs) reading every `removing` mark with
//!   `mark::live` and no lock, then `settle` locking the member and `finish` checking
//!   the Servers, moving the mark to `removed` only from `removing`, and purging only
//!   when it moved;
//! - the cancel: `cancel_removal` (removal/mod.rs) locking the Project and the
//!   Environment, reading the mark under them, and `stop` cancelling the removal
//!   Deployment and saving `cancelled`;
//! - a close: `removal::close` (removal/due.rs) locking the Environment, reading the
//!   mark under the lock, returning on a tombstone, and settling as the sweep does;
//! - the runner: `deployment::record` (deployment/lifecycle.rs) ending the removal
//!   Deployment under the Project lock, as one step, since it takes that lock before
//!   anything else and the cancel takes it first too. A running removal ends
//!   `Applied`; a cancelling one ends `Applied` when its run succeeded, or
//!   `Cancelled` when it failed (`end`).
//!
//! READ COMMITTED (storage/pg.rs `begin`): a statement reads the rows committed when
//! it runs, a row update waits for the transaction holding the row and then applies
//! to what it committed, and a copy a transaction read earlier stays as it was.
//! SQLite (storage/sqlite.rs `transaction`) takes the write lock at BEGIN, so there
//! each transaction runs whole.
//!
//! Out of scope: a Project-target mark and its members, a second sweep running
//! beside the first, Cloud admitting what the sweep lists as due, a `remove()` run
//! again, a queued removal Deployment, the `lock_timeout` that fails a wait past ten
//! seconds, the runner's own row lock on the Deployment, and the evidence and
//! consent columns `mark::save`'s upsert overwrites.

use std::fmt::Write as _;

use stateright::{Checker, HasDiscoveries, Model, Property};

/// `mark::State` (removal/mark.rs).
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum MarkState {
    Removing,
    Cancelled,
    Removed,
}

/// The removal Deployment `remove()` admitted, as `on_servers` (removal/mod.rs)
/// reads it: in flight, or the last that ran. Only an applied removal takes the
/// Environment off the Servers; a cancelled one leaves the Deploy before it there.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Removal {
    Running,
    Cancelling,
    Applied,
    Cancelled,
}

impl Removal {
    /// `on_servers(..).is_none()`: `ran` filters out only a removal that applied.
    fn off_the_servers(self) -> bool {
        self == Self::Applied
    }
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Actor {
    Sweep,
    Close,
    Cancel,
}

impl Actor {
    const ALL: [Self; 3] = [Self::Sweep, Self::Close, Self::Cancel];
}

/// One statement of a transaction, or one row lock it takes.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Step {
    /// `mark::live` (removal/mark.rs): every `removing` mark, read under no lock.
    ReadLive,
    /// `lock_project` (removal/mod.rs): `UPDATE config_project SET name = name`.
    LockProject,
    /// `scope::lock_all` (scope.rs): `UPDATE config_environment SET working_revision
    /// = working_revision WHERE id = ?`, which waits for whoever holds the row.
    LockEnvironment,
    /// `scope::owned` or `scope::load` after the lock: fails once the row is gone.
    LoadEnvironment,
    /// `mark::of` (removal/mark.rs), read under the locks taken before it.
    ReadMark,
    /// `on_servers` in `finish` (removal/due.rs): nothing to delete while any
    /// Deployment of the Environment may still run.
    CheckServers,
    /// `mark::finish` in `finish` (removal/due.rs): `UPDATE config_removal_mark SET
    /// state = 'removed' WHERE id = ? AND state = 'removing'`.
    SaveRemoved,
    /// `purge` in `finish` (removal/due.rs): `DELETE FROM config_environment`, only
    /// when `mark::finish` moved the mark.
    Purge,
    /// `stop` (removal/mod.rs): `in_flight`, then `deployment::cancel` on a removal
    /// Deployment still in flight.
    CancelRemoval,
    /// `mark::save` with `state = Cancelled` in `stop` (removal/mod.rs).
    SaveCancelled,
    Commit,
}

/// `removal::sweep` for one mark: `mark::live`, then `settle` and `finish`.
const SWEEP: &[Step] = &[
    Step::ReadLive,
    Step::LockEnvironment,
    Step::LoadEnvironment,
    Step::CheckServers,
    Step::SaveRemoved,
    Step::Purge,
    Step::Commit,
];

/// `removal::close`: the mark read under the member lock, then `settle` and `finish`.
const CLOSE: &[Step] = &[
    Step::LockEnvironment,
    Step::LoadEnvironment,
    Step::ReadMark,
    Step::CheckServers,
    Step::SaveRemoved,
    Step::Purge,
    Step::Commit,
];

/// `cancel_removal` for an Environment target, then `stop`.
const CANCEL: &[Step] = &[
    Step::LockProject,
    Step::LockEnvironment,
    Step::LoadEnvironment,
    Step::ReadMark,
    Step::CancelRemoval,
    Step::SaveCancelled,
    Step::Commit,
];

/// `cancel_removal` with `Broken::cancel_locks_no_environment`: `scope::environment`
/// loads the Environment without locking it, and `stop` locks no member.
const CANCEL_UNLOCKED: &[Step] = &[
    Step::LockProject,
    Step::LoadEnvironment,
    Step::ReadMark,
    Step::CancelRemoval,
    Step::SaveCancelled,
    Step::Commit,
];

/// How a transaction ended.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Outcome {
    /// Committed what it wrote.
    Committed,
    /// Committed having written nothing: no live mark, a tombstone, the Servers still
    /// busy, or a save that moved no row.
    Empty,
    /// Returned an error, which `Storage::write` rolls back.
    Failed,
}

/// One transaction's progress, and what it wrote but has not committed.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct Tx {
    /// Its next step, an index into its program.
    next: usize,
    outcome: Option<Outcome>,
    mark: Option<MarkState>,
    purge: bool,
    removal: Option<Removal>,
    /// Its `mark::finish` found the mark no longer `removing`.
    moved_nothing: bool,
}

impl Tx {
    const START: Self = Self {
        next: 0,
        outcome: None,
        mark: None,
        purge: false,
        removal: None,
        moved_nothing: false,
    };

    fn over(self) -> bool {
        self.outcome.is_some()
    }

    fn committed(self) -> bool {
        self.outcome == Some(Outcome::Committed)
    }
}

/// Who holds each row a transaction updates, from the update to its end.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq, Default)]
struct Held {
    project: Option<Actor>,
    environment: Option<Actor>,
    mark: Option<Actor>,
}

impl Held {
    fn release(&mut self, actor: Actor) {
        for holder in [&mut self.project, &mut self.environment, &mut self.mark] {
            if *holder == Some(actor) {
                *holder = None;
            }
        }
    }
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct State {
    /// The committed mark row.
    mark: MarkState,
    /// The committed `config_environment` row exists.
    environment: bool,
    /// The committed removal Deployment.
    removal: Removal,
    held: Held,
    sweep: Tx,
    close: Tx,
    cancel: Tx,
    /// The removal Deployment's status when the runner recorded how it ended.
    recorded: Option<Removal>,
    /// `cancel_removal` returned success to its caller.
    cancelled: bool,
}

impl State {
    fn tx(&self, actor: Actor) -> Tx {
        match actor {
            Actor::Sweep => self.sweep,
            Actor::Close => self.close,
            Actor::Cancel => self.cancel,
        }
    }

    fn tx_mut(&mut self, actor: Actor) -> &mut Tx {
        match actor {
            Actor::Sweep => &mut self.sweep,
            Actor::Close => &mut self.close,
            Actor::Cancel => &mut self.cancel,
        }
    }

    fn holder(&self, step: Step) -> Option<Option<Actor>> {
        match step {
            Step::LockProject => Some(self.held.project),
            Step::LockEnvironment => Some(self.held.environment),
            Step::SaveRemoved | Step::SaveCancelled => Some(self.held.mark),
            Step::ReadLive
            | Step::LoadEnvironment
            | Step::ReadMark
            | Step::CheckServers
            | Step::Purge
            | Step::CancelRemoval
            | Step::Commit => None,
        }
    }

    /// Whether `actor` can run `step` now: a row lock waits while another holds it.
    fn admits(&self, actor: Actor, step: Step) -> bool {
        self.holder(step)
            .is_none_or(|holder| holder.is_none_or(|holder| holder == actor))
    }

    fn end(&mut self, actor: Actor, outcome: Outcome) {
        self.held.release(actor);
        let tx = self.tx_mut(actor);
        tx.outcome = Some(outcome);
        tx.mark = None;
        tx.purge = false;
        tx.removal = None;
    }
}

/// Which Store adapter runs the transactions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Backend {
    /// Statements interleave; row locks wait (storage/pg.rs).
    Postgres,
    /// Each write transaction runs whole (storage/sqlite.rs).
    Sqlite,
}

/// Who may finish the mark beside the cancel and the runner.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Finishers {
    Sweep,
    Close,
    Both,
}

impl Finishers {
    fn runs(self, actor: Actor) -> bool {
        match (self, actor) {
            (_, Actor::Cancel)
            | (Self::Both, _)
            | (Self::Sweep, Actor::Sweep)
            | (Self::Close, Actor::Close) => true,
            (Self::Sweep, Actor::Close) | (Self::Close, Actor::Sweep) => false,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct Broken {
    /// `finish` saves `removed` as `mark::save`'s upsert does, `INSERT ... ON
    /// CONFLICT (id) DO UPDATE SET state = excluded.state` whatever state the row
    /// holds, and purges, as it did before `mark::finish`.
    finish_saves_unconditionally: bool,
    /// `close()` goes on past a `Cancelled` mark (removal/due.rs) as if it were live.
    close_ignores_tombstone: bool,
    /// `cancel_removal` loads the Environment with `scope::environment` instead of
    /// `scope::lock`, and `stop` skips `scope::lock_id` on each member.
    cancel_locks_no_environment: bool,
}

#[derive(Clone, Copy, Debug)]
struct RemovalMarks {
    backend: Backend,
    finishers: Finishers,
    broken: Broken,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Action {
    /// `Actor` runs its next statement, `Step`.
    Step(Actor, Step),
    /// The runner records the removal Deployment's end with this status.
    Record(Removal),
    /// On SQLite, `Actor`'s transaction runs whole.
    Whole(Actor),
}

impl RemovalMarks {
    fn program(&self, actor: Actor) -> &'static [Step] {
        match actor {
            Actor::Sweep => SWEEP,
            Actor::Close => CLOSE,
            Actor::Cancel if self.broken.cancel_locks_no_environment => CANCEL_UNLOCKED,
            Actor::Cancel => CANCEL,
        }
    }

    fn next_step(&self, state: &State, actor: Actor) -> Option<Step> {
        let tx = state.tx(actor);
        if tx.over() {
            return None;
        }
        self.program(actor).get(tx.next).copied()
    }

    /// The statuses `deployment::record` may end the removal with now. It takes the
    /// Project lock first, then the Deployment, which only a removal still running
    /// or cancelling has to record. `record` picks `Applied` when the run succeeded
    /// and `Failed` otherwise, and `end` turns only `(Cancelling, Failed)` into
    /// `Cancelled` (deployment/lifecycle.rs). A running removal that fails is out of
    /// bounds: it leaves the Environment on the Servers, and no finisher gets past
    /// `CheckServers`.
    fn records(state: &State) -> &'static [Removal] {
        if state.recorded.is_some() || !state.environment || state.held.project.is_some() {
            return &[];
        }
        match state.removal {
            Removal::Running => &[Removal::Applied],
            Removal::Cancelling => &[Removal::Applied, Removal::Cancelled],
            Removal::Applied | Removal::Cancelled => &[],
        }
    }

    fn record(state: &mut State, ended: Removal) {
        state.recorded = Some(state.removal);
        state.removal = ended;
    }

    /// `actor` runs the step at its program counter, which `admits` already allowed.
    fn step(&self, state: &mut State, actor: Actor) {
        let Some(step) = self.next_step(state, actor) else {
            return;
        };
        let advance = |state: &mut State| state.tx_mut(actor).next += 1;
        match step {
            Step::ReadLive => {
                if state.mark == MarkState::Removing {
                    advance(state);
                } else {
                    state.end(actor, Outcome::Empty);
                }
            }
            Step::LockProject => {
                state.held.project = Some(actor);
                advance(state);
            }
            Step::LockEnvironment => {
                state.held.environment = Some(actor);
                advance(state);
            }
            Step::LoadEnvironment => {
                if state.environment {
                    advance(state);
                } else {
                    state.end(actor, Outcome::Failed);
                }
            }
            Step::ReadMark => match state.mark {
                MarkState::Removing => advance(state),
                MarkState::Cancelled
                    if actor == Actor::Close && self.broken.close_ignores_tombstone =>
                {
                    advance(state);
                }
                // `cancel_removal` answers "isn't being removed"; `close` returns Ok.
                MarkState::Cancelled | MarkState::Removed => {
                    let outcome = if actor == Actor::Cancel {
                        Outcome::Failed
                    } else {
                        Outcome::Empty
                    };
                    state.end(actor, outcome);
                }
            },
            Step::CheckServers => {
                if state.removal.off_the_servers() {
                    advance(state);
                } else {
                    state.end(actor, Outcome::Empty);
                }
            }
            Step::SaveRemoved => {
                state.held.mark = Some(actor);
                let live = state.mark == MarkState::Removing;
                let unconditional = self.broken.finish_saves_unconditionally;
                let tx = state.tx_mut(actor);
                if live || unconditional {
                    tx.mark = Some(MarkState::Removed);
                } else {
                    tx.moved_nothing = true;
                }
                advance(state);
            }
            Step::Purge => {
                let unconditional = self.broken.finish_saves_unconditionally;
                let tx = state.tx_mut(actor);
                tx.purge = unconditional || tx.mark.is_some();
                advance(state);
            }
            Step::CancelRemoval => {
                let cancelling = match state.removal {
                    Removal::Running => Some(Removal::Cancelling),
                    Removal::Cancelling | Removal::Applied | Removal::Cancelled => None,
                };
                state.tx_mut(actor).removal = cancelling;
                advance(state);
            }
            Step::SaveCancelled => {
                state.held.mark = Some(actor);
                state.tx_mut(actor).mark = Some(MarkState::Cancelled);
                advance(state);
            }
            Step::Commit => {
                let tx = state.tx(actor);
                if let Some(mark) = tx.mark {
                    state.mark = mark;
                }
                if tx.purge {
                    state.environment = false;
                }
                if let Some(removal) = tx.removal {
                    state.removal = removal;
                }
                let wrote = tx.mark.is_some() || tx.purge || tx.removal.is_some();
                let outcome = if wrote {
                    Outcome::Committed
                } else {
                    Outcome::Empty
                };
                state.end(actor, outcome);
                if actor == Actor::Cancel && tx.mark == Some(MarkState::Cancelled) {
                    state.cancelled = true;
                }
            }
        }
    }

    fn at_rest(&self, state: &State) -> bool {
        let mut actions = Vec::new();
        self.actions(state, &mut actions);
        actions.is_empty()
    }
}

impl Model for RemovalMarks {
    type State = State;
    type Action = Action;

    fn init_states(&self) -> Vec<State> {
        [Removal::Running, Removal::Applied]
            .into_iter()
            .map(|removal| State {
                mark: MarkState::Removing,
                environment: true,
                removal,
                held: Held::default(),
                sweep: Tx::START,
                close: Tx::START,
                cancel: Tx::START,
                recorded: None,
                cancelled: false,
            })
            .collect()
    }

    fn actions(&self, state: &State, actions: &mut Vec<Action>) {
        for ended in Self::records(state) {
            actions.push(Action::Record(*ended));
        }
        for actor in Actor::ALL
            .into_iter()
            .filter(|actor| self.finishers.runs(*actor))
        {
            let Some(step) = self.next_step(state, actor) else {
                continue;
            };
            match self.backend {
                Backend::Postgres if state.admits(actor, step) => {
                    actions.push(Action::Step(actor, step));
                }
                Backend::Postgres => {}
                Backend::Sqlite if state.tx(actor).next == 0 => {
                    actions.push(Action::Whole(actor));
                }
                Backend::Sqlite => {}
            }
        }
    }

    fn next_state(&self, last: &State, action: Action) -> Option<State> {
        let mut state = last.clone();
        match action {
            Action::Step(actor, _) => self.step(&mut state, actor),
            Action::Record(ended) => Self::record(&mut state, ended),
            Action::Whole(actor) => {
                while !state.tx(actor).over() {
                    self.step(&mut state, actor);
                }
            }
        }
        Some(state)
    }

    fn properties(&self) -> Vec<Property<Self>> {
        let mut properties = vec![
            Property::always(CANCEL_KEPT, |_, state: &State| {
                !state.cancelled || (state.mark != MarkState::Removed && state.environment)
            }),
            Property::always(EVERY_TX_ENDS, |model: &Self, state: &State| {
                !model.at_rest(state)
                    || Actor::ALL
                        .into_iter()
                        .filter(|actor| model.finishers.runs(*actor))
                        .all(|actor| state.tx(actor).over())
            }),
            Property::sometimes("a cancel returns success", |_, state: &State| {
                state.cancelled
            }),
            Property::sometimes(
                "a cancel stops the removal Deployment still running",
                |_, state: &State| {
                    state.cancelled
                        && matches!(state.removal, Removal::Cancelling | Removal::Cancelled)
                },
            ),
            Property::sometimes(
                "a cancel returns success while the removal applied on the Servers",
                |_, state: &State| {
                    state.cancelled
                        && state.recorded == Some(Removal::Cancelling)
                        && state.removal == Removal::Applied
                },
            ),
            Property::sometimes(
                "a cancel fails because the Environment is already purged",
                |_, state: &State| {
                    state.cancel.outcome == Some(Outcome::Failed) && !state.environment
                },
            ),
        ];
        if self.finishers.runs(Actor::Sweep) {
            properties.extend([
                Property::sometimes(
                    "the sweep finishes the removal and purges its Environment",
                    |_, state: &State| state.sweep.committed() && !state.environment,
                ),
                Property::sometimes(
                    "the sweep finds no live mark after a cancel committed",
                    |_, state: &State| {
                        state.cancelled
                            && state.sweep.outcome == Some(Outcome::Empty)
                            && state.mark == MarkState::Cancelled
                    },
                ),
            ]);
        }
        if self.finishers.runs(Actor::Close) {
            properties.push(Property::sometimes(
                "a close finishes the removal and purges its Environment",
                |_, state: &State| state.close.committed() && !state.environment,
            ));
            if !self.broken.close_ignores_tombstone {
                properties.push(Property::sometimes(
                    "a close returns on the tombstone",
                    |_, state: &State| {
                        state.close.outcome == Some(Outcome::Empty)
                            && state.mark == MarkState::Cancelled
                    },
                ));
            }
        }
        if self.backend == Backend::Postgres
            && !self.broken.finish_saves_unconditionally
            && self.finishers.runs(Actor::Sweep)
        {
            properties.push(Property::sometimes(
                "the sweep's save finds the mark no longer removing and purges nothing",
                |_, state: &State| state.sweep.moved_nothing && state.environment,
            ));
        }
        properties
    }
}

const CANCEL_KEPT: &str = "a cancel that returned success keeps its Environment";
const EVERY_TX_ENDS: &str = "every transaction ends";

/// `path` under `model` in the code's words: each statement, and the committed rows
/// and held locks once it ran.
fn story(model: &RemovalMarks, path: stateright::Path<State, Action>) -> String {
    let mut story = String::new();
    // Stateright pairs each state with the action taken from it; the row lists the
    // action that led to the state instead, so the last row shows the broken state.
    let mut led_by = None;
    // The committed removal Deployment before the step that led here.
    let mut before = None;
    for (n, (state, action)) in path.into_vec().into_iter().enumerate() {
        let what = match led_by {
            None => "start".to_owned(),
            Some(Action::Record(ended)) => format!(
                "runner: deployment::record ends the removal Deployment {ended:?} under the Project lock"
            ),
            Some(Action::Whole(actor)) => format!("{actor:?}: the whole transaction, serialized"),
            Some(Action::Step(actor, step)) => {
                format!("{actor:?}: {}", reads_as(model, actor, step, before))
            }
        };
        led_by = action;
        before = Some(state.removal);
        let held =
            |holder: Option<Actor>| holder.map_or_else(|| "free".to_owned(), |h| format!("{h:?}"));
        let _ = writeln!(
            story,
            "{n:2}. {what}\n    mark={:?} environment={} removal={:?} held: project={} environment={} mark={} cancelled={}",
            state.mark,
            if state.environment {
                "present"
            } else {
                "purged"
            },
            state.removal,
            held(state.held.project),
            held(state.held.environment),
            held(state.held.mark),
            state.cancelled,
        );
    }
    story
}

/// `step` as `actor` runs it under `model`, with `removal` the committed removal
/// Deployment it saw.
fn reads_as(
    model: &RemovalMarks,
    actor: Actor,
    step: Step,
    removal: Option<Removal>,
) -> &'static str {
    let broken = model.broken;
    match step {
        Step::ReadLive => {
            "mark::live reads every `removing` mark, under no lock (removal/due.rs sweep)"
        }
        Step::LockProject => "lock_project updates the Project row (removal/mod.rs cancel_removal)",
        Step::LockEnvironment => {
            "scope::lock_all updates the Environment row, waiting for its holder (scope.rs)"
        }
        Step::LoadEnvironment => match actor {
            Actor::Sweep | Actor::Close => {
                "scope::lock_id loads the Environment with owned after the lock, or the write fails (scope.rs)"
            }
            Actor::Cancel if broken.cancel_locks_no_environment => {
                "scope::environment loads the Environment, unlocked, or the write fails (scope.rs)"
            }
            Actor::Cancel => {
                "scope::lock loads the Environment with load after the lock, or the write fails (scope.rs)"
            }
        },
        Step::ReadMark => "mark::of reads the mark under the locks (removal/mark.rs)",
        Step::CheckServers => {
            "finish asks on_servers whether anything of the Environment may still run (removal/due.rs)"
        }
        Step::SaveRemoved if broken.finish_saves_unconditionally => {
            "finish saves the mark it read as `removed`, whatever the row now holds (removal/due.rs, mark::save's upsert, under Broken)"
        }
        Step::SaveRemoved => {
            "finish moves the mark to `removed` only from `removing` (removal/due.rs, mark::finish's conditional UPDATE)"
        }
        Step::Purge if broken.finish_saves_unconditionally => {
            "finish purges the Environment (removal/due.rs, removal::purge, under Broken)"
        }
        Step::Purge => {
            "finish purges the Environment if its UPDATE moved the mark (removal/due.rs, removal::purge)"
        }
        Step::CancelRemoval => match removal {
            Some(Removal::Running) => {
                "stop finds the removal Deployment in flight and cancels it (removal/mod.rs)"
            }
            _ => "stop finds no removal Deployment it can cancel (removal/mod.rs)",
        },
        Step::SaveCancelled => "stop saves the mark as `cancelled` (removal/mod.rs, mark::save)",
        Step::Commit => "COMMIT: every row it updated is released",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(model: RemovalMarks) -> impl Checker<RemovalMarks> {
        model.checker().threads(4).spawn_bfs().join()
    }

    fn as_is(finishers: Finishers) -> RemovalMarks {
        RemovalMarks {
            backend: Backend::Postgres,
            finishers,
            broken: Broken::default(),
        }
    }

    /// A path on which `property` fails under `model`, printed as a story.
    fn breaks(model: RemovalMarks, property: &'static str) -> State {
        let checker = model
            .checker()
            .threads(4)
            .finish_when(HasDiscoveries::AllOf([property].into()))
            .spawn_bfs()
            .join();
        let path = checker.assert_any_discovery(property);
        let last = path.last_state().clone();
        println!("{property} fails under {model:?}:\n{}", story(&model, path));
        last
    }

    /// The finding, now kept out. With `finish` saving `removed` whatever the row
    /// holds, as it did before `mark::finish`, the sweep reads the mark before the
    /// cancel commits and takes the member lock after it, then saves its stale copy as
    /// `removed` and purges the Environment the cancel just kept.
    #[test]
    fn finish_needs_its_conditional_save() {
        let last = breaks(
            RemovalMarks {
                broken: Broken {
                    finish_saves_unconditionally: true,
                    ..Broken::default()
                },
                ..as_is(Finishers::Sweep)
            },
            CANCEL_KEPT,
        );
        assert!(last.cancelled, "the cancel returned success");
        assert_eq!(last.mark, MarkState::Removed);
        assert!(!last.environment, "the Environment is purged");
    }

    /// `close` reads the mark under the member lock, after any cancel that holds it
    /// committed, so a close alone never undoes a cancel.
    #[test]
    fn a_close_reads_the_mark_under_the_member_lock_and_keeps_a_cancel() {
        let checker = check(as_is(Finishers::Close));
        println!("close, as is: {} states", checker.unique_state_count());
        checker.assert_properties();
    }

    /// SQLite runs each write transaction whole, so the sweep sees the cancel or the
    /// cancel sees the purge; the code as it is holds there.
    #[test]
    fn sqlite_runs_each_transaction_whole_and_keeps_a_cancel() {
        let checker = check(RemovalMarks {
            backend: Backend::Sqlite,
            ..as_is(Finishers::Both)
        });
        println!("sqlite, as is: {} states", checker.unique_state_count());
        checker.assert_properties();
    }

    /// `finish` moves the row to `removed` only from `removing`, and purges only
    /// when a row moved. Every property holds with the sweep, a close, the cancel
    /// and the runner interleaved, and every `sometimes` is reached: a cancel
    /// commits, a removal finishes and purges, and the sweep and the cancel land in
    /// both orders.
    #[test]
    fn a_conditional_save_in_finish_keeps_a_committed_cancel() {
        let checker = check(as_is(Finishers::Both));
        println!("postgres, as is: {} states", checker.unique_state_count());
        checker.assert_properties();
    }

    /// Without the `Cancelled` early return in `close()`, a pull-request close after
    /// a cancel settles the tombstone as if it were live. `mark::finish` then moves
    /// nothing, so the check shows its worth for this property only with `finish`
    /// saving unconditionally too: the close then purges the Environment. With the
    /// check in place, that save alone keeps every property, so the pair is what
    /// breaks it.
    #[test]
    fn close_needs_its_tombstone_check() {
        let unconditional = Broken {
            finish_saves_unconditionally: true,
            ..Broken::default()
        };
        let checker = check(RemovalMarks {
            broken: unconditional,
            ..as_is(Finishers::Close)
        });
        println!(
            "close, unconditional save: {} states",
            checker.unique_state_count()
        );
        checker.assert_properties();

        let last = breaks(
            RemovalMarks {
                broken: Broken {
                    close_ignores_tombstone: true,
                    ..unconditional
                },
                ..as_is(Finishers::Close)
            },
            CANCEL_KEPT,
        );
        assert!(last.cancelled);
        assert!(!last.environment);
    }

    /// Without the Environment lock in `cancel_removal` and `stop`, the cancel reads
    /// a live mark while the sweep is purging, waits for the sweep at the mark row,
    /// and commits `cancelled` over an Environment that is already gone. The fix to
    /// `finish` does not cover this: the guard is the lock.
    #[test]
    fn cancel_needs_the_environment_lock() {
        let last = breaks(
            RemovalMarks {
                broken: Broken {
                    cancel_locks_no_environment: true,
                    ..Broken::default()
                },
                ..as_is(Finishers::Sweep)
            },
            CANCEL_KEPT,
        );
        assert!(last.cancelled);
        assert!(!last.environment);
    }
}
