//! One Machine upgrade attempt, and what Machine mutation admission answers around it. The
//! daemon's `request_locked` (ployzd installer/upgrade.rs:170-239) holds the installation
//! lock, writes the Accepted receipt (`upgrade-attempt.json`, :227), writes the
//! `.upgrade-active` marker (`mark_active`, :228, mutation.rs:107-111) and launches the
//! worker as a transient systemd unit (:230). If the launch fails, it writes
//! Failed{Launching} (:235) and then clears the marker (:236). The worker (`work`,
//! :299-346) waits for the lock (:306), refuses an attempt that is not its own or is
//! already terminal (:308-310), writes Running (:316), installs, writes the terminal
//! receipt (:343) and then clears the marker (:344). `try_mutation` (mutation.rs:55-69)
//! answers Busy while another process holds the lock (:57) or the marker exists (:58-62).
//!
//! Four processes take part. The daemon runs `request_locked` in a task (:156-158) and hosts
//! every RPC: Inspect, a resend, Leave, a new upgrade request and the mutations that admit
//! through `admit_installation` (local_machine.rs:331-334). A kill ends all of them at once.
//! systemd restarts it (`Restart=always`, installer/host.rs:296), and daemon start
//! reconciles before it serves (daemon.rs:184-186). The worker is its own unit (:397-459),
//! `ployzd install` its own process, and the volume plugin its own unit
//! (installer/host.rs:359-382), which admits through `admit_mutation`
//! (volume_plugin/storage.rs:115-124).
//!
//! Daemon start clears the marker only if its `settle` wins `try_installation` (:258-260).
//! Otherwise `settle` returns Ok (:263), `reconcile` drops it (:349-351) and the daemon
//! serves (daemon.rs:187). `ployzd install` runs beside the daemon's reader, and a worker
//! that finds its receipt terminal holds the lock until it returns `NotActive` (:306-309),
//! so either can hold the lock while daemon start settles.
//!
//! Settling readers repair a receipt and marker that a stopped writer left. `settle`
//! (installer/upgrade.rs:251-276) runs for Inspect (local_machine.rs:512, through `latest`,
//! :109-117), for a resend of the same attempt (local_machine/upgrade.rs:27-31, through
//! `replay`, :90-105), for Leave (local_machine.rs:945) and at daemon start (daemon.rs:184,
//! through `reconcile`, :349-351). `reconcile_locked` (:372-395) runs under the lock for a
//! new upgrade request (:185, after local_machine/upgrade.rs:38) and for `ployzd install`
//! (`reconcile_for_install`, :353-370, from `install_at` in installer/mod.rs).
//!
//! The finding: every terminal write comes before its `clear_active`. A process killed
//! between the two, or a `clear_active` that returns Io before it removes the marker
//! (mutation.rs:116, :119), releases the lock with the marker still naming a finished
//! attempt, and `try_mutation` answers Busy on the marker alone. The window lasts until a
//! settling reader wins `try_installation` and clears the marker. `reconcile_locked` has the
//! same order: it writes Interrupted (:392) before its `clear_active` (:393).
//!
//! Every write of the receipt or the marker happens under the installation lock: the
//! request's (`request`, :156-158), the worker's (:306), `settle`'s (:258, :268) and the
//! callers of `reconcile_locked` above. `try_mutation` reads the marker while holding the
//! lock shared (:57-58), so no write lands between its two reads. Readers that run without
//! the lock (`settle`'s first receipt read and its `worker_state`, :265) are separate model
//! steps, and another process can step between them.
//!
//! The fix variant reads the receipt only beside the marker: Busy while another process
//! holds the lock, or while `.upgrade-active` exists and the receipt is not terminal. It
//! still writes and clears the marker as the code does. The model's older reader stands for
//! a release before the fix, which gates on the marker alone, and the fix does not help it:
//! it still refuses on a stale marker until a settling reader clears it.

use std::fmt::Write as _;

use ployz_core::{MachineUpgradeOutcome, MachineUpgradeStage, MachineVersion};
use stateright::{Model, Property};

use crate::harness::{self, Variant, everywhere};

/// The outcome `upgrade-attempt.json` holds (installer/upgrade.rs `StoredAttempt`). The model
/// has one attempt, so the receipt and the marker always name it.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Outcome {
    /// `request_locked` wrote it (:227).
    Accepted,
    /// The worker wrote it (:316, and the progress writes, :323-331).
    Running,
    /// The worker's terminal write (:343). Its Failed write is the same statement.
    Succeeded,
    /// `request_locked`'s Failed{Launching} (:231-235).
    Failed,
    /// `reconcile_locked` found the worker stopped (:384-392).
    Interrupted,
}

impl Outcome {
    /// `MachineUpgradeOutcome::is_terminal` on this outcome.
    fn is_terminal(self) -> bool {
        let stage = MachineUpgradeStage::Launching;
        match self {
            Self::Accepted => MachineUpgradeOutcome::Accepted,
            Self::Running => MachineUpgradeOutcome::Running { stage },
            Self::Succeeded => MachineUpgradeOutcome::Succeeded {
                version: MachineVersion::parse("1.2.3").expect("a literal version"),
            },
            Self::Failed => MachineUpgradeOutcome::Failed {
                stage,
                error: String::new(),
            },
            Self::Interrupted => MachineUpgradeOutcome::Interrupted { stage },
        }
        .is_terminal()
    }
}

/// Who holds the installation lock exclusively (`.install.lock`, mutation.rs:77-92). A kill
/// releases it, since the lock is held on an open file (:37, :156-160).
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Holder {
    Request,
    Worker,
    /// A settling reader the daemon hosts.
    Reader,
    /// `ployzd install`.
    Install,
}

/// The daemon process.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Daemon {
    /// Serves RPCs.
    Serving,
    /// Killed or exited; systemd starts it again (`Restart=always`, `RestartSec=2`,
    /// installer/host.rs:296-298).
    Down,
    /// Daemon start's `reconcile` runs (daemon.rs:184-186); nothing is served yet.
    Starting,
}

/// Where the daemon's acceptance task is in `request_locked`.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Request {
    /// No request has run.
    Idle,
    /// Holds the lock and wrote Accepted (:227); `mark_active` (:228) is next.
    Accepted,
    /// Wrote the marker; `launch_worker` (:230) is next.
    Marked,
    /// Under Broken: `launch_worker` started the unit before `mark_active`, which is next.
    Launched,
    /// `launch_worker` failed; the Failed{Launching} write (:235) is next.
    LaunchFailed,
    /// Wrote Failed{Launching}; `clear_active` (:236) is next.
    FailedWritten,
    /// Returned, was refused, or ended with the daemon; the lock it held is released.
    Done,
}

/// The worker unit (`ployz-upgrade-<id>.service`, installer/upgrade.rs:397-459) and where its
/// `work` is.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Worker {
    /// No unit was started.
    Absent,
    /// The unit runs and waits in `lock_installation` (:306).
    Waiting,
    /// Holds the lock and read a terminal receipt (:306-308); it returns `NotActive` next
    /// (:309), and its guard holds the lock until then.
    Refusing,
    /// Holds the lock and wrote Running (:316); installing.
    Holding,
    /// Wrote the terminal receipt (:343); `clear_active` (:344) is next.
    Wrote,
    /// The process exited or was killed; the unit is inactive or gone.
    Stopped,
}

impl Worker {
    /// `worker_state` reads Active (:467-518): the unit runs.
    fn active(self) -> bool {
        matches!(
            self,
            Self::Waiting | Self::Refusing | Self::Holding | Self::Wrote
        )
    }
}

/// A caller of `settle` or `reconcile_locked`.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Caller {
    /// The Inspect RPC (local_machine.rs:463, :512), through `latest`.
    Inspect,
    /// A resend of the same attempt (local_machine/upgrade.rs:27-31), through `replay`. The
    /// CLI's poll (ployz handlers/server/upgrade.rs poll_upgrade at :80-98, :700-701) and Cloud's resend
    /// (dashboard server-upgrade.server.ts:288-292) send it.
    Replay,
    /// Leave's `depart` (local_machine.rs:938-961), through `latest`, then `finish_mutation`.
    Leave,
    /// Daemon start (daemon.rs:184), through `reconcile` and `latest`, before it serves.
    DaemonStart,
    /// A request for another attempt: `replay` returns None for its id (:98-99), then
    /// `try_installation` (local_machine/upgrade.rs:38) and `request_locked`'s
    /// `reconcile_locked` (:185).
    NewRequest,
    /// `ployzd install`'s `install_at` (installer/mod.rs), its own process.
    Install,
}

impl Caller {
    const ALL: [Self; 6] = [
        Self::Inspect,
        Self::Replay,
        Self::Leave,
        Self::DaemonStart,
        Self::NewRequest,
        Self::Install,
    ];

    /// Reaches `settle` rather than `reconcile_locked` under its own lock.
    fn settles(self) -> bool {
        matches!(
            self,
            Self::Inspect | Self::Replay | Self::Leave | Self::DaemonStart
        )
    }

    /// Runs in the daemon process.
    fn in_daemon(self) -> bool {
        self != Self::Install
    }

    /// Who holds the lock when this caller does.
    fn holder(self) -> Holder {
        if self.in_daemon() {
            Holder::Reader
        } else {
            Holder::Install
        }
    }
}

/// Where a settling reader is. `seen` is the receipt `settle`'s caller read before it took
/// the lock, which `settle` returns when it does not take the lock.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum At {
    /// `settle` read a receipt that is not terminal; `worker_state` (:265) is next.
    QueryWorker { seen: Outcome },
    /// `settle` read a terminal receipt; `try_installation` (:261) is next.
    TryTerminal { seen: Outcome },
    /// `settle` found the unit stopped; `try_installation` (:268) is next.
    TryNonTerminal { seen: Outcome },
    /// Holds the lock; `clear_active` is next.
    Clear(Site),
    /// Holds the lock; `reconcile_locked` returned and the caller decides: a new request at
    /// :186-193, `ployzd install` at :357-369.
    Decide,
    /// Leave's `settle` returned; `depart` checks whether the attempt is live
    /// (local_machine.rs:946-953) and otherwise runs `finish_mutation`'s `try_mutation`
    /// (:959, :355, :333). `repaired` says Leave's own `settle` removed the marker a
    /// stopped writer left.
    Depart { live: bool, repaired: bool },
}

/// Which `clear_active` a settling reader runs.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Site {
    /// `settle`'s, for a receipt it read terminal (:259).
    Settle,
    /// `reconcile_locked`'s, for a terminal receipt (:378).
    Terminal,
    /// `reconcile_locked`'s, after it wrote Interrupted (:393).
    Interrupted,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct Reader {
    caller: Caller,
    at: At,
}

impl Reader {
    fn holds_lock(self) -> bool {
        matches!(self.at, At::Clear(_) | At::Decide)
    }
}

/// Whose `try_mutation` answered.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Asker {
    /// A daemon RPC, through `admit_installation` (local_machine.rs:331-334) or the Update
    /// fast path (:851). It runs only while the daemon serves.
    Daemon,
    /// The volume plugin's `admit_mutation` (volume_plugin/storage.rs:121), its own unit.
    Plugin,
    /// The fix: a release before it, which gates on the marker as the code as it is does.
    Older,
}

/// What `try_mutation` answered.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Answer {
    Busy,
    Admitted,
}

/// The process whose stop first left the lock free beside the marker of a finished attempt.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Stop {
    /// The daemon was killed: its request task between :235 and :236, or a reader it hosts
    /// between a write and its `clear_active`.
    Daemon,
    /// The worker was killed between :343 and :344.
    Worker,
    /// `ployzd install` was killed between :392 and :393.
    Install,
    /// A `clear_active` returned Io before it removed the marker (mutation.rs:116, :119).
    Io,
}

/// The branch the last step took.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Branch {
    /// `request_upgrade`'s `try_installation` refused (local_machine/upgrade.rs:38).
    RequestBusy,
    /// `clear_active` removed the marker after Failed{Launching} (:236).
    ClearsAfterLaunchFailure,
    /// `clear_active` found no marker and returned Ok, removing nothing (mutation.rs:118).
    ClearsNothing,
    /// The worker found its receipt terminal and returned `NotActive` (:308-310).
    WorkerNotActive,
    /// `request_locked` found no receipt and `.upgrade-active`, and refused (:191-192).
    RequestFindsMarker,
    /// The worker cleared the marker after its terminal write (:344).
    WorkerClears,
    /// `clear_active` returned Io before removing the marker (mutation.rs:116, :119), and the
    /// caller returned it with `?`.
    ClearFails,
    /// `clear_active` removed the marker, then its directory sync returned Io
    /// (mutation.rs:121), and the caller returned it with `?`.
    ClearFailsAfterRemoval,
    /// `settle`'s caller found no receipt (:113-115, :95-97).
    NoReceipt,
    /// `settle` returned a running worker's receipt as it stands (:265-266).
    SettleSeesActive,
    /// `settle`'s `try_installation` answered Busy, and `settle` returned the receipt it
    /// read, without clearing (:260 or :270).
    SettleBusy,
    /// `settle` removed the marker of a terminal receipt (:259).
    SettleClears,
    /// `reconcile_locked` found the unit running and returned (:381-382).
    ReconcileSeesActive,
    /// `reconcile_locked` wrote Interrupted (:384-392).
    Interrupts,
    /// `reconcile_locked` removed the marker (:378 or :393).
    ReconcileClears,
    /// `depart` refused while the attempt is not terminal (local_machine.rs:946-953).
    LeaveRefuses,
    /// `depart` ran `finish_mutation` after Leave's own `settle` removed the marker a
    /// stopped writer left.
    DepartsAfterItsClear,
    /// The caller's `try_installation` answered Busy (local_machine/upgrade.rs:38,
    /// `install_at` in installer/mod.rs).
    CallerBusy,
    /// A new request refused after reconciling (:186-187 or :191-192).
    NewRequestBusy,
    /// A new request passed :191 and goes on to resolve its release, out of the model.
    NewRequestGoesOn,
    /// `reconcile_for_install` refused (:358-359 or :368), and `install_at` returned Busy
    /// (`map_upgrade_reconciliation` in installer/mod.rs).
    InstallBusy,
    /// `install_at` goes on to `install_locked` (`install_at` in installer/mod.rs).
    InstallGoesOn,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Action {
    /// `request_upgrade`: `replay` finds no receipt for the id, `try_installation` takes the
    /// lock (local_machine/upgrade.rs:27-38), and `request_locked` finds no receipt and no
    /// marker (:177, :191), resolves the release and writes Accepted (:227).
    Accept,
    /// `mark_active` (:228).
    Mark,
    /// `launch_worker` returns Ok (:230): `systemd-run` started the unit. The task returns
    /// and drops the lock (:238, :157).
    Launch,
    /// `launch_worker` returns an error (:230). `started` says the unit started anyway:
    /// `systemd-run` timed out after 20 s (:22, :440-447) after systemd created the unit.
    LaunchFails { started: bool },
    /// The Failed{Launching} write (:231-235).
    WriteLaunchFailure,
    /// `clear_active` after Failed{Launching} (:236).
    ClearAfterLaunchFailure,
    /// The daemon process is killed. Its acceptance task and every reader it hosts end, and
    /// their locks are released. `started` says `systemd-run`, killed with it while
    /// `launch_worker` ran, had started the unit.
    DaemonDies { started: bool },
    /// `lock_installation` returns (:306); the worker reads the receipt (:307) and either
    /// finds it terminal (:308) or writes Running (:316).
    WorkerLocks,
    /// The worker returns `NotActive` (:309) and the process exits; its guard drops.
    WorkerRefuses,
    /// The install ends and the worker writes the terminal receipt (:318-343).
    WorkerFinishes,
    /// `clear_active` after the terminal write (:344); the process exits.
    WorkerClears,
    /// The worker process is killed: by a kill, a crash or the unit's `RuntimeMaxSec=15min`
    /// (:21, :414).
    WorkerDies,
    /// A `clear_active` of the lock holder returns Io (mutation.rs:113-123): at :236, :344,
    /// :259, :378 or :393. `removed` says it failed at the directory sync (:121), after
    /// `remove_file` (:116); otherwise it failed reading or removing the marker (:116,
    /// :119), which stays.
    ClearFails { removed: bool },
    /// A reader starts: `settle`'s caller reads the receipt, or a locking caller tries the
    /// lock and runs `reconcile_locked`'s reads and write.
    Start(Caller),
    /// The reader the daemon hosts takes its next step.
    ReaderStep,
    /// `ployzd install` takes its next step.
    InstallStep,
    /// `ployzd install` is killed; any lock it held is released.
    InstallDies,
    /// A `try_mutation` by `asker`.
    Mutate(Asker),
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct State {
    /// `upgrade-attempt.json`.
    receipt: Option<Outcome>,
    /// `.upgrade-active` names the attempt.
    marker: bool,
    holder: Option<Holder>,
    daemon: Daemon,
    request: Request,
    worker: Worker,
    /// The settling reader the daemon hosts, if any. The model runs one at a time.
    reader: Option<Reader>,
    /// `ployzd install`, if it runs. It runs beside the daemon's reader.
    install: Option<Reader>,
    /// Who ran `try_mutation` and what it answered, on the step that ran it.
    answer: Option<(Asker, Answer)>,
    /// Whose stop first left the lock free with the marker naming a terminal receipt. The
    /// marker is written once, so it stays until a `clear_active` removes it.
    stranded: Option<Stop>,
    /// The caller whose `clear_active` removed the marker a stopped writer left, on the step
    /// that removed it.
    cleared: Option<Caller>,
    /// The serving daemon's start found the lock held: its `settle` returned without
    /// clearing (:260 or :270).
    start_lost: bool,
    /// The first terminal outcome the receipt held.
    first_terminal: Option<Outcome>,
    took: Option<Branch>,
}

impl State {
    /// The slot `caller` runs in: the daemon's reader, or `ployzd install`.
    fn slot(&self, caller: Caller) -> Option<Reader> {
        if caller.in_daemon() {
            self.reader
        } else {
            self.install
        }
    }

    fn slot_mut(&mut self, caller: Caller) -> &mut Option<Reader> {
        if caller.in_daemon() {
            &mut self.reader
        } else {
            &mut self.install
        }
    }

    fn receipt_terminal(&self) -> bool {
        self.receipt.is_some_and(Outcome::is_terminal)
    }

    fn receipt_live(&self) -> bool {
        self.receipt.is_some_and(|outcome| !outcome.is_terminal())
    }

    fn lock_free(&self) -> bool {
        self.holder.is_none()
    }

    fn answered(&self, asker: Asker, answer: Answer) -> bool {
        self.answer == Some((asker, answer))
    }

    /// The release the model runs answered, through the daemon or the plugin.
    fn current_answered(&self, answer: Answer) -> bool {
        self.answered(Asker::Daemon, answer) || self.answered(Asker::Plugin, answer)
    }

    fn admitted(&self) -> bool {
        matches!(self.answer, Some((_, Answer::Admitted)))
    }

    /// The lock is free and the marker of a finished attempt a stopped writer left is still
    /// there.
    fn stale_marker(&self) -> bool {
        self.lock_free() && self.marker && self.receipt_terminal()
    }
}

/// A guard of the code as it is taken out, to show the check catches what it prevents.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each flag removes one independent guard"
)]
struct Broken {
    /// `try_mutation` takes no shared lock (mutation.rs:57), so it does not see another
    /// process holding the lock.
    mutation_ignores_lock: bool,
    /// `try_mutation` reads no marker (mutation.rs:58-62). Under the fix this is every
    /// marker read in admission.
    mutation_ignores_marker: bool,
    /// `request_locked` launches the worker (:230) before `mark_active` (:228).
    launches_before_marking: bool,
    /// The worker writes Running over a terminal receipt: no `is_terminal` check at :308.
    worker_ignores_terminal: bool,
    /// `settle` runs `reconcile_locked` on the receipt it read before the lock, with no
    /// re-read under it (:273).
    settle_reuses_snapshot: bool,
    /// `settle` takes the lock for a live receipt without its `worker_state` check (:265).
    settle_skips_worker_state: bool,
    /// `reconcile_locked` writes Interrupted without its `worker_state` check (:381).
    reconcile_skips_worker_state: bool,
    /// The daemon serves again after a kill without daemon start's `reconcile`
    /// (daemon.rs:184-186).
    serves_before_reconcile: bool,
}

/// A part of the fix taken out, to show the check catches what it prevents.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Fix {
    /// The fix's admission reads the marker alone, as the code as it is does.
    ignores_receipt: bool,
    /// The fix's admission reads the receipt whether or not the marker exists.
    skips_marker_gate: bool,
    /// The fix writes no marker.
    writes_no_marker: bool,
}

/// One `sometimes`: a branch the code handles, read from the state that took it. A row is
/// reached only on the code as it is when it is a refusal the fix removes.
type Coverage = harness::Coverage<UpgradeAttempt>;

fn took(state: &State, branch: Branch) -> bool {
    state.took == Some(branch)
}

/// Each branch the bounds reach, on the code as it is, on the fix, or on both.
/// `current_code_breaks_only_the_finding` and `the_fix_answers_from_the_receipt` expect
/// every row their model reaches.
const COVERAGE: &[Coverage] = &[
    Coverage {
        name: "try_mutation refuses while the worker holds the installation lock",
        on: everywhere,
        omit: None,
        reached: |_, state| {
            state.current_answered(Answer::Busy) && state.holder == Some(Holder::Worker)
        },
    },
    Coverage {
        name: "try_mutation refuses between the request dropping the lock and the worker taking it",
        on: everywhere,
        omit: None,
        reached: |_, state| {
            state.current_answered(Answer::Busy)
                && state.lock_free()
                && state.worker == Worker::Waiting
                && state.receipt == Some(Outcome::Accepted)
        },
    },
    Coverage {
        name: "the worker clears the marker after its terminal write",
        on: everywhere,
        omit: None,
        reached: |_, state| took(state, Branch::WorkerClears) && !state.marker,
    },
    Coverage {
        name: "request_locked clears the marker after Failed{Launching}",
        on: everywhere,
        omit: None,
        reached: |_, state| {
            took(state, Branch::ClearsAfterLaunchFailure)
                && !state.marker
                && state.receipt == Some(Outcome::Failed)
        },
    },
    Coverage {
        name: "the worker refuses an attempt whose receipt is already terminal",
        on: everywhere,
        omit: None,
        reached: |_, state| took(state, Branch::WorkerNotActive) && state.receipt_terminal(),
    },
    Coverage {
        name: "settle returns a running worker's receipt as it stands",
        on: everywhere,
        omit: None,
        reached: |_, state| took(state, Branch::SettleSeesActive) && state.worker.active(),
    },
    Coverage {
        name: "settle leaves the marker of a terminal receipt while the worker holds the lock",
        on: everywhere,
        omit: None,
        reached: |_, state| {
            took(state, Branch::SettleBusy)
                && state.receipt_terminal()
                && state.marker
                && state.holder == Some(Holder::Worker)
        },
    },
    Coverage {
        name: "reconcile_locked writes Interrupted for a stopped worker",
        on: everywhere,
        omit: None,
        reached: |_, state| {
            took(state, Branch::Interrupts)
                && state.receipt == Some(Outcome::Interrupted)
                && !state.worker.active()
        },
    },
    Coverage {
        name: "reconcile_locked leaves a running worker's receipt as it stands",
        on: everywhere,
        omit: None,
        reached: |_, state| {
            took(state, Branch::ReconcileSeesActive)
                && state.worker.active()
                && state.receipt_live()
        },
    },
    Coverage {
        name: "clear_active fails before removing the marker of a finished attempt",
        on: everywhere,
        omit: None,
        reached: |_, state| took(state, Branch::ClearFails) && state.stale_marker(),
    },
    Coverage {
        name: "clear_active removes the marker and then fails its directory sync",
        on: everywhere,
        omit: None,
        reached: |_, state| {
            took(state, Branch::ClearFailsAfterRemoval) && !state.marker && state.lock_free()
        },
    },
    Coverage {
        name: "a clear_active that finds no marker removes nothing",
        on: everywhere,
        omit: None,
        reached: |_, state| took(state, Branch::ClearsNothing) && !state.marker,
    },
    Coverage {
        name: "Inspect's settle removes the marker a stopped writer left",
        on: everywhere,
        omit: None,
        reached: |_, state| state.cleared == Some(Caller::Inspect),
    },
    Coverage {
        name: "a resend's settle removes the marker a stopped writer left",
        on: everywhere,
        omit: None,
        reached: |_, state| state.cleared == Some(Caller::Replay),
    },
    Coverage {
        name: "Leave's settle removes the marker a stopped writer left",
        on: everywhere,
        omit: None,
        reached: |_, state| state.cleared == Some(Caller::Leave),
    },
    Coverage {
        name: "daemon start's settle removes the marker a stopped writer left",
        on: everywhere,
        omit: None,
        reached: |_, state| state.cleared == Some(Caller::DaemonStart),
    },
    Coverage {
        name: "a new request's reconcile_locked removes the marker a stopped writer left",
        on: everywhere,
        omit: None,
        reached: |_, state| state.cleared == Some(Caller::NewRequest),
    },
    Coverage {
        name: "ployzd install's reconcile_locked removes the marker a stopped writer left",
        on: everywhere,
        omit: None,
        reached: |_, state| state.cleared == Some(Caller::Install),
    },
    Coverage {
        name: "Leave is admitted after its own settle removed the marker a stopped writer left",
        on: everywhere,
        omit: None,
        reached: |_, state| {
            took(state, Branch::DepartsAfterItsClear)
                && state.answered(Asker::Daemon, Answer::Admitted)
        },
    },
    Coverage {
        name: "Leave refuses while the attempt is live",
        on: everywhere,
        omit: None,
        reached: |_, state| took(state, Branch::LeaveRefuses) && state.receipt_live(),
    },
    Coverage {
        name: "a new request refuses while the attempt is live",
        on: everywhere,
        omit: None,
        reached: |_, state| took(state, Branch::NewRequestBusy) && state.receipt_live(),
    },
    Coverage {
        name: "a new request goes on after a finished attempt",
        on: everywhere,
        omit: None,
        reached: |_, state| took(state, Branch::NewRequestGoesOn) && state.receipt_terminal(),
    },
    Coverage {
        name: "ployzd install refuses while the attempt is live",
        on: everywhere,
        omit: None,
        reached: |_, state| took(state, Branch::InstallBusy) && state.receipt_live(),
    },
    Coverage {
        name: "ployzd install goes on after a finished attempt",
        on: everywhere,
        omit: None,
        reached: |_, state| took(state, Branch::InstallGoesOn) && state.receipt_terminal(),
    },
    Coverage {
        name: "a caller's try_installation refuses while another process holds the lock",
        on: everywhere,
        omit: None,
        reached: |_, state| took(state, Branch::CallerBusy) && !state.lock_free(),
    },
    Coverage {
        name: "request_locked refuses .upgrade-active with no receipt",
        on: everywhere,
        omit: None,
        reached: |_, state| {
            took(state, Branch::RequestFindsMarker) && state.marker && state.receipt.is_none()
        },
    },
    Coverage {
        name: "try_mutation refuses on .upgrade-active with no receipt",
        on: everywhere,
        omit: None,
        reached: |_, state| {
            state.current_answered(Answer::Busy)
                && state.lock_free()
                && state.marker
                && state.receipt.is_none()
        },
    },
    Coverage {
        name: "the volume plugin refuses on the marker a killed daemon left while the daemon is down",
        on: |model| model.fix.is_shipped(),
        omit: None,
        reached: |_, state| {
            state.answered(Asker::Plugin, Answer::Busy)
                && state.stale_marker()
                && state.stranded == Some(Stop::Daemon)
                && state.daemon == Daemon::Down
        },
    },
    Coverage {
        name: "a daemon RPC refuses on the marker a killed daemon left after daemon start lost try_installation",
        on: |model| model.fix.is_shipped(),
        omit: None,
        reached: |_, state| {
            state.answered(Asker::Daemon, Answer::Busy)
                && state.stale_marker()
                && state.stranded == Some(Stop::Daemon)
                && state.start_lost
        },
    },
    Coverage {
        name: "a daemon RPC refuses on the marker a killed worker left",
        on: |model| model.fix.is_shipped(),
        omit: None,
        reached: |_, state| {
            state.answered(Asker::Daemon, Answer::Busy)
                && state.stale_marker()
                && state.stranded == Some(Stop::Worker)
        },
    },
    Coverage {
        name: "the fix admits beside the marker of a finished attempt",
        on: |model| model.fix.is_proposed(),
        omit: None,
        reached: |_, state| state.current_answered(Answer::Admitted) && state.stale_marker(),
    },
    Coverage {
        name: "the fix refuses on the marker of an attempt whose worker stopped before a terminal write",
        on: |model| model.fix.is_proposed(),
        omit: None,
        reached: |_, state| {
            state.current_answered(Answer::Busy)
                && state.lock_free()
                && state.marker
                && state.receipt_live()
                && state.worker == Worker::Stopped
        },
    },
    Coverage {
        name: "an older reader refuses on the marker of a finished attempt under the fix",
        on: |model| model.fix.is_proposed(),
        omit: None,
        reached: |_, state| state.answered(Asker::Older, Answer::Busy) && state.stale_marker(),
    },
];

#[derive(Clone, Copy, Debug)]
struct UpgradeAttempt {
    /// The fix: the receipt read beside the marker; the marker kept as it is written today.
    fix: Variant<Fix>,
    broken: Broken,
}

impl UpgradeAttempt {
    /// The proposal's parts taken out, none on the shipped code.
    fn proposal(&self) -> Fix {
        self.fix.proposal().unwrap_or_default()
    }

    /// `try_mutation` (mutation.rs:55-69) as `asker` runs it. The shared lock first (:57).
    /// The code as it is, and a release before the fix, then read the marker (:58-62); the
    /// fix reads the receipt when the marker exists, and refuses unless it is terminal, so a
    /// marker with no receipt still refuses. Every receipt and marker write holds the lock
    /// exclusively, so neither changes while `try_mutation` holds it shared.
    fn admission(&self, state: &State, asker: Asker) -> Answer {
        let broken = self.broken;
        let locked = !state.lock_free() && !broken.mutation_ignores_lock;
        let marker = state.marker && !broken.mutation_ignores_marker;
        let busy = if self.fix.is_proposed() && asker != Asker::Older {
            if self.proposal().skips_marker_gate {
                state.receipt_live()
            } else if self.proposal().ignores_receipt {
                marker
            } else {
                marker && !state.receipt_terminal()
            }
        } else {
            marker
        };
        if locked || busy {
            Answer::Busy
        } else {
            Answer::Admitted
        }
    }

    fn release(state: &mut State) {
        state.holder = None;
    }

    /// `clear_active` by the lock holder: it removes the marker when the marker names the
    /// attempt (mutation.rs:116) and returns Ok, removing nothing, when it finds none
    /// (:118). Says whether it removed one.
    fn clear(state: &mut State) -> bool {
        let removed = state.marker;
        state.marker = false;
        removed
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one arm per step of the processes the model runs"
    )]
    fn step(&self, state: &mut State, action: Action) {
        let broken = self.broken;
        state.took = None;
        state.answer = None;
        state.cleared = None;
        match action {
            Action::Accept => {
                if !state.lock_free() {
                    state.request = Request::Done;
                    state.took = Some(Branch::RequestBusy);
                } else if state.marker {
                    state.request = Request::Done;
                    state.took = Some(Branch::RequestFindsMarker);
                } else {
                    state.holder = Some(Holder::Request);
                    state.receipt = Some(Outcome::Accepted);
                    state.request = Request::Accepted;
                }
            }
            Action::Mark => {
                if !self.proposal().writes_no_marker {
                    state.marker = true;
                }
                if state.request == Request::Launched {
                    state.request = Request::Done;
                    Self::release(state);
                } else {
                    state.request = Request::Marked;
                }
            }
            Action::Launch => {
                state.worker = Worker::Waiting;
                if broken.launches_before_marking {
                    state.request = Request::Launched;
                } else {
                    state.request = Request::Done;
                    Self::release(state);
                }
            }
            Action::LaunchFails { started } => {
                if started {
                    state.worker = Worker::Waiting;
                }
                state.request = Request::LaunchFailed;
            }
            Action::WriteLaunchFailure => {
                state.receipt = Some(Outcome::Failed);
                state.request = Request::FailedWritten;
            }
            Action::ClearAfterLaunchFailure => {
                let removed = Self::clear(state);
                state.request = Request::Done;
                Self::release(state);
                state.took = Some(if removed {
                    Branch::ClearsAfterLaunchFailure
                } else {
                    Branch::ClearsNothing
                });
            }
            Action::DaemonDies { started } => {
                if started {
                    state.worker = Worker::Waiting;
                }
                if state.holder == Some(Holder::Request) {
                    Self::release(state);
                }
                state.request = Request::Done;
                if state.reader.is_some_and(Reader::holds_lock) {
                    Self::release(state);
                }
                state.reader = None;
                state.start_lost = false;
                state.daemon = if broken.serves_before_reconcile {
                    Daemon::Serving
                } else {
                    Daemon::Down
                };
            }
            Action::WorkerLocks => {
                state.holder = Some(Holder::Worker);
                if state.receipt_terminal() && !broken.worker_ignores_terminal {
                    state.worker = Worker::Refusing;
                } else {
                    state.receipt = Some(Outcome::Running);
                    state.worker = Worker::Holding;
                }
            }
            Action::WorkerRefuses => {
                state.worker = Worker::Stopped;
                Self::release(state);
                state.took = Some(Branch::WorkerNotActive);
            }
            Action::WorkerFinishes => {
                state.receipt = Some(Outcome::Succeeded);
                state.worker = Worker::Wrote;
            }
            Action::WorkerClears => {
                let removed = Self::clear(state);
                state.worker = Worker::Stopped;
                Self::release(state);
                state.took = Some(if removed {
                    Branch::WorkerClears
                } else {
                    Branch::ClearsNothing
                });
            }
            Action::WorkerDies => {
                if state.holder == Some(Holder::Worker) {
                    Self::release(state);
                }
                state.worker = Worker::Stopped;
            }
            Action::ClearFails { removed } => {
                if removed {
                    state.marker = false;
                }
                match state.holder {
                    Some(Holder::Request) => state.request = Request::Done,
                    Some(Holder::Worker) => state.worker = Worker::Stopped,
                    Some(Holder::Reader) => {
                        // Daemon start returns the error, and the daemon exits
                        // (daemon.rs:184-186).
                        if state
                            .reader
                            .is_some_and(|reader| reader.caller == Caller::DaemonStart)
                        {
                            state.daemon = Daemon::Down;
                        }
                        state.reader = None;
                    }
                    Some(Holder::Install) => state.install = None,
                    None => unreachable!("only the lock holder clears"),
                }
                Self::release(state);
                state.took = Some(if removed {
                    Branch::ClearFailsAfterRemoval
                } else {
                    Branch::ClearFails
                });
            }
            Action::Start(caller) => self.start(state, caller),
            Action::ReaderStep => self.reader_step(state, state.reader.expect("a reader")),
            Action::InstallStep => {
                self.reader_step(state, state.install.expect("ployzd install runs"));
            }
            Action::InstallDies => {
                if state.holder == Some(Holder::Install) {
                    Self::release(state);
                }
                state.install = None;
            }
            Action::Mutate(asker) => {
                state.answer = Some((asker, self.admission(state, asker)));
            }
        }
    }

    /// A reader's first step. `settle`'s callers read the receipt (`latest`, :113;
    /// `replay`, :95). A new request and `ployzd install` try the lock and, holding it, run
    /// `reconcile_locked`.
    fn start(&self, state: &mut State, caller: Caller) {
        if !caller.settles() {
            if state.lock_free() {
                self.reconcile_locked(state, caller, state.receipt);
            } else {
                state.took = Some(Branch::CallerBusy);
            }
            return;
        }
        if caller == Caller::DaemonStart {
            state.daemon = Daemon::Starting;
        }
        match state.receipt {
            None => {
                state.took = Some(Branch::NoReceipt);
                Self::settled(state, caller, None, false);
            }
            Some(seen) => {
                let at = if seen.is_terminal() {
                    At::TryTerminal { seen }
                } else {
                    At::QueryWorker { seen }
                };
                *state.slot_mut(caller) = Some(Reader { caller, at });
            }
        }
    }

    /// Holding the lock, `reconcile_locked` (:372-395) on `read`, the receipt its caller
    /// read: under the lock (:177, :273, :357), or, under Broken, `settle`'s read from
    /// before it. The lock keeps every receipt and marker writer out, and the model starts a
    /// unit only at a launch step that holds the lock, so the read, `worker_state` and the
    /// Interrupted write are one step.
    fn reconcile_locked(&self, state: &mut State, caller: Caller, read: Option<Outcome>) {
        state.holder = Some(caller.holder());
        let at = match read {
            None => At::Decide,
            Some(outcome) if outcome.is_terminal() => At::Clear(Site::Terminal),
            Some(_) if state.worker.active() && !self.broken.reconcile_skips_worker_state => {
                state.took = Some(Branch::ReconcileSeesActive);
                At::Decide
            }
            Some(_) => {
                state.receipt = Some(Outcome::Interrupted);
                state.took = Some(Branch::Interrupts);
                At::Clear(Site::Interrupted)
            }
        };
        if caller.settles() && at == At::Decide {
            // `settle` returns what `reconcile_locked` left (:274-275), and the lock drops.
            Self::release(state);
            Self::settled(state, caller, state.receipt, false);
        } else {
            *state.slot_mut(caller) = Some(Reader { caller, at });
        }
    }

    /// `settle` returned `outcome` to its caller. Daemon start then serves; Leave goes on to
    /// `depart`'s check.
    fn settled(state: &mut State, caller: Caller, outcome: Option<Outcome>, repaired: bool) {
        if caller == Caller::DaemonStart {
            state.daemon = Daemon::Serving;
        }
        state.reader = (caller == Caller::Leave).then_some(Reader {
            caller,
            at: At::Depart {
                live: outcome.is_some_and(|outcome| !outcome.is_terminal()),
                repaired,
            },
        });
    }

    fn reader_step(&self, state: &mut State, reader: Reader) {
        match reader.at {
            At::QueryWorker { seen } => {
                if state.worker.active() && !self.broken.settle_skips_worker_state {
                    state.took = Some(Branch::SettleSeesActive);
                    Self::settled(state, reader.caller, Some(seen), false);
                } else {
                    *state.slot_mut(reader.caller) = Some(Reader {
                        caller: reader.caller,
                        at: At::TryNonTerminal { seen },
                    });
                }
            }
            At::TryTerminal { seen } => {
                if state.lock_free() {
                    state.holder = Some(Holder::Reader);
                    *state.slot_mut(reader.caller) = Some(Reader {
                        caller: reader.caller,
                        at: At::Clear(Site::Settle),
                    });
                } else {
                    Self::settle_busy(state, reader.caller, seen);
                }
            }
            At::TryNonTerminal { seen } => {
                if state.lock_free() {
                    let read = if self.broken.settle_reuses_snapshot {
                        Some(seen)
                    } else {
                        state.receipt
                    };
                    self.reconcile_locked(state, reader.caller, read);
                } else {
                    Self::settle_busy(state, reader.caller, seen);
                }
            }
            At::Clear(site) => {
                let stranded = state.stranded.is_some();
                let removed = Self::clear(state);
                if removed && stranded {
                    state.cleared = Some(reader.caller);
                }
                state.took = Some(match (removed, site) {
                    (false, _) => Branch::ClearsNothing,
                    (true, Site::Settle) => Branch::SettleClears,
                    (true, Site::Terminal | Site::Interrupted) => Branch::ReconcileClears,
                });
                if reader.caller.settles() {
                    Self::release(state);
                    let repaired = state.cleared == Some(Caller::Leave);
                    Self::settled(state, reader.caller, state.receipt, repaired);
                } else {
                    *state.slot_mut(reader.caller) = Some(Reader {
                        caller: reader.caller,
                        at: At::Decide,
                    });
                }
            }
            At::Decide => {
                let busy = match (reader.caller, state.receipt) {
                    // :186-187, then `active` at :191-192.
                    (Caller::NewRequest, receipt) => {
                        receipt.is_some_and(|outcome| !outcome.is_terminal()) || state.marker
                    }
                    // `active` with no receipt (:357-362), else a live receipt (:365-369).
                    (_, None) => state.marker,
                    (_, Some(outcome)) => !outcome.is_terminal(),
                };
                state.took = Some(match (reader.caller, busy) {
                    (Caller::NewRequest, true) => Branch::NewRequestBusy,
                    (Caller::NewRequest, false) => Branch::NewRequestGoesOn,
                    (_, true) => Branch::InstallBusy,
                    (_, false) => Branch::InstallGoesOn,
                });
                Self::release(state);
                *state.slot_mut(reader.caller) = None;
            }
            At::Depart { live: true, .. } => {
                state.took = Some(Branch::LeaveRefuses);
                state.reader = None;
            }
            At::Depart {
                live: false,
                repaired,
            } => {
                state.answer = Some((Asker::Daemon, self.admission(state, Asker::Daemon)));
                if repaired {
                    state.took = Some(Branch::DepartsAfterItsClear);
                }
                state.reader = None;
            }
        }
    }

    /// `settle`'s `try_installation` answered Busy (:260, :270): it returns the receipt it
    /// read.
    fn settle_busy(state: &mut State, caller: Caller, seen: Outcome) {
        state.took = Some(Branch::SettleBusy);
        if caller == Caller::DaemonStart {
            state.start_lost = true;
        }
        Self::settled(state, caller, Some(seen), false);
    }
}

impl Model for UpgradeAttempt {
    type State = State;
    type Action = Action;

    /// A serving daemon with no receipt and no marker, before the Machine's first upgrade
    /// request. And the same with `.upgrade-active` and no receipt, which no step writes
    /// (`mark_active`, :228, follows the Accepted write, :227) but `try_mutation` and
    /// `reconcile_for_install` refuse (:357-362), as ployzd's
    /// `machine_api/local_tests.rs:188` and `build/tests.rs:451-453` write it.
    fn init_states(&self) -> Vec<State> {
        let start = State {
            receipt: None,
            marker: false,
            holder: None,
            daemon: Daemon::Serving,
            request: Request::Idle,
            worker: Worker::Absent,
            reader: None,
            install: None,
            answer: None,
            stranded: None,
            cleared: None,
            start_lost: false,
            first_terminal: None,
            took: None,
        };
        vec![
            start,
            State {
                marker: true,
                ..start
            },
        ]
    }

    /// The request's steps, the daemon's kill, the worker's steps, the Io failures, a
    /// reader's steps, then `try_mutation`. The order decides only which of the shortest
    /// paths a story shows.
    fn actions(&self, state: &State, actions: &mut Vec<Action>) {
        let serving = state.daemon == Daemon::Serving;
        let launches = [
            Action::Launch,
            Action::LaunchFails { started: false },
            Action::LaunchFails { started: true },
        ];
        match state.request {
            Request::Idle if serving => actions.push(Action::Accept),
            Request::Accepted if self.broken.launches_before_marking => actions.extend(launches),
            Request::Accepted | Request::Launched => actions.push(Action::Mark),
            Request::Marked => actions.extend(launches),
            Request::LaunchFailed => actions.push(Action::WriteLaunchFailure),
            Request::FailedWritten => actions.push(Action::ClearAfterLaunchFailure),
            Request::Idle | Request::Done => {}
        }
        if state.daemon != Daemon::Down && state.request != Request::Idle {
            actions.push(Action::DaemonDies { started: false });
        }
        if state.request == Request::Marked {
            actions.push(Action::DaemonDies { started: true });
        }
        match state.worker {
            Worker::Waiting if state.lock_free() => actions.push(Action::WorkerLocks),
            Worker::Refusing => actions.push(Action::WorkerRefuses),
            Worker::Holding => actions.push(Action::WorkerFinishes),
            Worker::Wrote => actions.push(Action::WorkerClears),
            Worker::Absent | Worker::Waiting | Worker::Stopped => {}
        }
        if state.worker.active() {
            actions.push(Action::WorkerDies);
        }
        let clearing = state.request == Request::FailedWritten
            || state.worker == Worker::Wrote
            || [state.reader, state.install]
                .into_iter()
                .flatten()
                .any(|reader| matches!(reader.at, At::Clear(_)));
        if clearing && state.marker {
            actions.extend([
                Action::ClearFails { removed: false },
                Action::ClearFails { removed: true },
            ]);
        }
        if state.reader.is_some() {
            actions.push(Action::ReaderStep);
        }
        actions.extend(
            Caller::ALL
                .into_iter()
                .filter(|caller| match caller {
                    Caller::DaemonStart => state.reader.is_none() && state.daemon == Daemon::Down,
                    Caller::Install => state.install.is_none(),
                    Caller::Inspect | Caller::Replay | Caller::Leave | Caller::NewRequest => {
                        state.reader.is_none() && serving
                    }
                })
                .map(Action::Start),
        );
        if state.install.is_some() {
            actions.extend([Action::InstallStep, Action::InstallDies]);
        }
        if serving {
            actions.push(Action::Mutate(Asker::Daemon));
        }
        actions.push(Action::Mutate(Asker::Plugin));
        if self.fix.is_proposed() {
            actions.push(Action::Mutate(Asker::Older));
        }
    }

    fn next_state(&self, last: &State, action: Action) -> Option<State> {
        let mut state = *last;
        self.step(&mut state, action);
        if state.stranded.is_none() && state.stale_marker() {
            state.stranded = Some(match action {
                Action::DaemonDies { .. } => Stop::Daemon,
                Action::WorkerDies => Stop::Worker,
                Action::InstallDies => Stop::Install,
                Action::ClearFails { .. } => Stop::Io,
                other @ (Action::Accept
                | Action::Mark
                | Action::Launch
                | Action::LaunchFails { .. }
                | Action::WriteLaunchFailure
                | Action::ClearAfterLaunchFailure
                | Action::WorkerLocks
                | Action::WorkerRefuses
                | Action::WorkerFinishes
                | Action::WorkerClears
                | Action::Start(_)
                | Action::ReaderStep
                | Action::InstallStep
                | Action::Mutate(_)) => {
                    unreachable!("{other:?} left a finished attempt's marker unlocked")
                }
            });
        }
        if state.first_terminal.is_none() && state.receipt_terminal() {
            state.first_terminal = state.receipt;
        }
        Some(state)
    }

    fn properties(&self) -> Vec<Property<Self>> {
        let mut properties = vec![
            Property::always(BUSY_ONLY_WHILE_LIVE, |_, state: &State| {
                !state.current_answered(Answer::Busy)
                    || !state.lock_free()
                    || !state.receipt_terminal()
            }),
            Property::always(NO_MUTATION_BESIDE_A_LIVE_WORKER, |_, state: &State| {
                !(state.admitted() && state.worker.active() && state.receipt_live())
            }),
            Property::always(NO_MUTATION_UNDER_THE_LOCK, |_, state: &State| {
                !(state.admitted() && !state.lock_free())
            }),
            Property::always(RECEIPT_FINAL, |_, state: &State| {
                state.first_terminal.is_none() || state.receipt == state.first_terminal
            }),
            Property::always(INTERRUPTED_ONLY_STOPPED, |_, state: &State| {
                !(took(state, Branch::Interrupts) && state.worker.active())
            }),
            Property::always(DAEMON_START_SETTLES_FIRST, |_, state: &State| {
                !(state.answered(Asker::Daemon, Answer::Busy)
                    && state.stale_marker()
                    && state.stranded == Some(Stop::Daemon))
                    || state.start_lost
            }),
            Property::always(NO_NEW_REFUSAL, |_, state: &State| {
                !state.current_answered(Answer::Busy) || !state.lock_free() || state.marker
            }),
        ];
        properties.extend(harness::sometimes(COVERAGE, self));
        properties
    }
}

const BUSY_ONLY_WHILE_LIVE: &str = "try_mutation answers Busy only while a process holds the installation lock or the receipt is not terminal";
const NO_MUTATION_BESIDE_A_LIVE_WORKER: &str =
    "no try_mutation admits while the worker's unit runs and the receipt is not terminal";
const NO_MUTATION_UNDER_THE_LOCK: &str =
    "no try_mutation admits while a process holds the installation lock";
const RECEIPT_FINAL: &str = "once the receipt is terminal, nothing changes it";
const INTERRUPTED_ONLY_STOPPED: &str =
    "reconcile_locked writes Interrupted only while the worker's unit is stopped";
const DAEMON_START_SETTLES_FIRST: &str = "a daemon RPC answers Busy on the marker a killed daemon left only after daemon start's settle lost try_installation";
const NO_NEW_REFUSAL: &str = "try_mutation answers Busy only while a process holds the installation lock or .upgrade-active exists";

/// `path` in the code's words: each step, then the receipt, the marker, the lock, the
/// daemon, the request, the worker, the daemon's reader and `ployzd install`.
fn story(model: &UpgradeAttempt, path: &[(State, Option<Action>)]) -> String {
    let mut story = String::new();
    // Stateright pairs each state with the action taken from it; the row lists the
    // action that led to the state instead, so the last row shows the broken state.
    let mut led_by: Option<(State, Action)> = None;
    for (n, (state, action)) in path.iter().enumerate() {
        let what = led_by.map_or_else(
            || {
                if state.marker {
                    "start: .upgrade-active and no receipt, as ployzd's machine_api/local_tests.rs:188 writes it; the daemon serves".to_owned()
                } else {
                    "start: no receipt, no marker, no upgrade requested; the daemon serves"
                        .to_owned()
                }
            },
            |(before, action)| reads_as(model, &before, action, state),
        );
        // A row that skips a guard is reached by a flag or by a direct edit to the model;
        // it names Broken only when a flag is set.
        let what = if model.broken == Broken::default() {
            what.replace(" (under Broken)", "")
        } else {
            what
        };
        led_by = action.map(|action| (*state, action));
        let _ = writeln!(
            story,
            "{n:2}. {what}\n    receipt: {} | marker: {} | lock: {} | daemon: {:?} | request: {:?} | worker: {:?} | reader: {} | install: {}",
            state
                .receipt
                .map_or_else(|| "none".to_owned(), |outcome| format!("{outcome:?}")),
            if state.marker { "names it" } else { "none" },
            state
                .holder
                .map_or_else(|| "free".to_owned(), |holder| format!("{holder:?}")),
            state.daemon,
            state.request,
            state.worker,
            state.reader.map_or_else(
                || "none".to_owned(),
                |reader| format!("{:?} at {:?}", reader.caller, reader.at)
            ),
            state
                .install
                .map_or_else(|| "none".to_owned(), |install| format!("{:?}", install.at)),
        );
    }
    story
}

/// Where each settling caller enters, in the code's words.
fn enters(caller: Caller) -> &'static str {
    match caller {
        Caller::Inspect => "Inspect (local_machine.rs:512) calls latest",
        Caller::Replay => "a resend of the same attempt (local_machine/upgrade.rs:28) calls replay",
        Caller::Leave => "Leave's depart (local_machine.rs:945) calls latest",
        Caller::DaemonStart => {
            "daemon start (daemon.rs:184) calls reconcile, then latest, before it serves"
        }
        Caller::NewRequest => {
            "a request for another attempt: replay returns None for its id (:98-99), and try_installation (local_machine/upgrade.rs:38)"
        }
        Caller::Install => "ployzd install's install_at: try_installation (installer::install_at)",
    }
}

/// `action` from `before` under `model`, read with `after` for the branch it took.
#[expect(
    clippy::too_many_lines,
    reason = "one arm per statement of the code the model copies"
)]
fn reads_as(model: &UpgradeAttempt, before: &State, action: Action, after: &State) -> String {
    match action {
        Action::Accept if after.took == Some(Branch::RequestBusy) => {
            "Daemon: request_upgrade's try_installation answers Busy; the request returns \"a Ployz installation or upgrade is active\" (local_machine/upgrade.rs:38, machine_api/local.rs:1069-1074)".to_owned()
        }
        Action::Accept if after.took == Some(Branch::RequestFindsMarker) => {
            "Daemon: request_upgrade takes the installation lock (local_machine/upgrade.rs:38); request_locked finds no receipt, finds .upgrade-active and returns Busy, and the lock drops (installer/upgrade.rs:177, :191-192)".to_owned()
        }
        Action::Accept => {
            "Daemon: request_upgrade takes the installation lock (local_machine/upgrade.rs:38); request_locked finds no receipt and no marker, and writes Accepted (installer/upgrade.rs:177, :191, :227)".to_owned()
        }
        Action::Mark if before.request == Request::Launched => format!(
            "Daemon: mark_active {} after the launch (under Broken); request_locked returns Accepted and the task drops the lock (installer/upgrade.rs:228, :238)",
            marks(after)
        ),
        Action::Mark => format!(
            "Daemon: mark_active {} (installer/upgrade.rs:228)",
            marks(after)
        ),
        Action::Launch if after.request == Request::Launched => {
            "Daemon: launch_worker starts the unit before mark_active (under Broken); the task still holds the lock (installer/upgrade.rs:230)".to_owned()
        }
        Action::Launch => {
            "Daemon: launch_worker starts the unit; request_locked returns Accepted and the task drops the lock (installer/upgrade.rs:230, :238, :157)".to_owned()
        }
        Action::LaunchFails { started: false } => {
            "Daemon: launch_worker returns an error and no unit started (installer/upgrade.rs:230)".to_owned()
        }
        Action::LaunchFails { started: true } => {
            "Daemon: launch_worker times out after 20 s, but systemd started the unit; its worker waits for the lock (installer/upgrade.rs:230, :440-447)".to_owned()
        }
        Action::WriteLaunchFailure => {
            "Daemon: request_locked writes Failed{Launching} (installer/upgrade.rs:231-235)".to_owned()
        }
        Action::ClearAfterLaunchFailure => format!(
            "Daemon: clear_active {}; request_locked returns and the lock drops (installer/upgrade.rs:236-238)",
            clears(after)
        ),
        Action::DaemonDies { started } => format!(
            "Daemon: the daemon process is killed{}{}; {}",
            match (before.request, started) {
                (Request::Accepted, _) => {
                    " while request_locked holds the lock, after the Accepted write and before mark_active (:227-228)"
                }
                (Request::Marked, false) => {
                    " while request_locked holds the lock, before launch_worker started the unit (:230)"
                }
                (Request::Marked, true) => {
                    " during launch_worker, after systemd started the unit, whose worker waits for the lock (:230)"
                }
                (Request::Launched, _) => {
                    " after the launch and before mark_active (under Broken)"
                }
                (Request::LaunchFailed, _) => {
                    " while request_locked holds the lock, before the Failed{Launching} write (:235)"
                }
                (Request::FailedWritten, _) => {
                    " while request_locked holds the lock, after the Failed{Launching} write and before clear_active (:235-236)"
                }
                (Request::Idle | Request::Done, _) => "",
            },
            before.reader.map_or_else(String::new, |reader| format!(
                "; the {:?} reader it hosts ends with it",
                reader.caller
            )),
            match (
                after.daemon,
                matches!(before.holder, Some(Holder::Request | Holder::Reader))
            ) {
                (Daemon::Serving, _) => {
                    "it serves again without daemon start's reconcile (under Broken)"
                }
                (_, true) => {
                    "its locks are released, and systemd starts it again (Restart=always, RestartSec=2, installer/host.rs:296-298)"
                }
                (_, false) => {
                    "it held no lock, and systemd starts it again (Restart=always, RestartSec=2, installer/host.rs:296-298)"
                }
            }
        ),
        Action::WorkerLocks if after.worker == Worker::Refusing => {
            "Worker: lock_installation returns; the receipt is terminal, so work returns NotActive next, and its guard holds the lock until then (installer/upgrade.rs:306-309)".to_owned()
        }
        Action::WorkerLocks if before.receipt_terminal() => {
            "Worker: lock_installation returns; the receipt is terminal, and work writes Running over it (under Broken) (installer/upgrade.rs:306-316)".to_owned()
        }
        Action::WorkerLocks => {
            "Worker: lock_installation returns; the receipt is this attempt's and not terminal; work writes Running (installer/upgrade.rs:306-316)".to_owned()
        }
        Action::WorkerRefuses => {
            "Worker: work returns NotActive; the process exits and the lock drops (installer/upgrade.rs:309)".to_owned()
        }
        Action::WorkerFinishes => {
            "Worker: the install ends; work writes the terminal receipt (installer/upgrade.rs:336-343)".to_owned()
        }
        Action::WorkerClears => format!(
            "Worker: clear_active {}; the process exits and the lock drops (installer/upgrade.rs:344-345)",
            clears(after)
        ),
        Action::WorkerDies => format!(
            "Worker: the process is killed (a kill, a crash, or RuntimeMaxSec=15min, installer/upgrade.rs:21, :414){}; the unit stops",
            match before.worker {
                Worker::Waiting => " while it waits for the lock (:306)",
                Worker::Refusing => " before it returns NotActive (:309), releasing the lock",
                Worker::Holding => " during the install, releasing the lock",
                Worker::Wrote => {
                    " after the terminal write and before clear_active (:343-344), releasing the lock"
                }
                Worker::Absent | Worker::Stopped => "",
            }
        ),
        Action::ClearFails { removed } => format!(
            "{}: {}, the caller returns it with ?, and the lock drops{}",
            match (before.holder, before.reader, before.install) {
                (Some(Holder::Request), ..) => "Daemon: request_locked's clear_active (installer/upgrade.rs:236)",
                (Some(Holder::Worker), ..) => "Worker: work's clear_active (installer/upgrade.rs:344)",
                (
                    Some(Holder::Reader),
                    Some(Reader {
                        at: At::Clear(Site::Settle),
                        ..
                    }),
                    _,
                ) => "Reader: settle's clear_active (installer/upgrade.rs:259)",
                (
                    Some(Holder::Reader),
                    Some(Reader {
                        at: At::Clear(Site::Interrupted),
                        ..
                    }),
                    _,
                ) => "Reader: reconcile_locked's clear_active, after Interrupted (installer/upgrade.rs:393)",
                (
                    Some(Holder::Install),
                    _,
                    Some(Reader {
                        at: At::Clear(Site::Interrupted),
                        ..
                    }),
                ) => "Install: reconcile_locked's clear_active, after Interrupted (installer/upgrade.rs:393)",
                (Some(Holder::Install), ..) => {
                    "Install: reconcile_locked's clear_active (installer/upgrade.rs:378)"
                }
                _ => "Reader: reconcile_locked's clear_active (installer/upgrade.rs:378)",
            },
            if removed {
                "removes .upgrade-active, then its directory sync returns Io (mutation.rs:116, :121)"
            } else {
                "returns Io before removing .upgrade-active, which stays (mutation.rs:116, :119)"
            },
            if before.daemon == Daemon::Starting && before.holder == Some(Holder::Reader) {
                "; daemon start fails and the daemon exits (daemon.rs:184-186)"
            } else {
                ""
            }
        ),
        Action::Start(caller) => format!(
            "{}: {}; {}",
            if caller.in_daemon() { "Reader" } else { "Install" },
            enters(caller),
            started(after, caller)
        ),
        Action::ReaderStep => {
            reader_step_reads_as(model, before.reader.expect("a reader steps"), before, after)
        }
        Action::InstallStep => reader_step_reads_as(
            model,
            before.install.expect("ployzd install steps"),
            before,
            after,
        )
        .replacen("Reader:", "Install:", 1),
        Action::InstallDies => {
            "Install: the ployzd install process is killed; any lock it held is released".to_owned()
        }
        Action::Mutate(Asker::Daemon) => format!(
            "Daemon RPC: admit_installation's try_mutation {} (local_machine.rs:331-334, mutation.rs:55-69)",
            answer_reads_as(model, before, after)
        ),
        Action::Mutate(Asker::Plugin) => format!(
            "Volume plugin: admit_mutation's try_mutation {} (volume_plugin/storage.rs:121, mutation.rs:55-69)",
            answer_reads_as(model, before, after)
        ),
        Action::Mutate(Asker::Older) => format!(
            "Older release: its try_mutation {} (mutation.rs:55-69 as the code is)",
            answer_reads_as(model, before, after)
        ),
    }
}

fn marks(after: &State) -> &'static str {
    if after.marker {
        "writes .upgrade-active"
    } else {
        "writes nothing (under Broken)"
    }
}

fn clears(after: &State) -> &'static str {
    if after.took == Some(Branch::ClearsNothing) {
        "finds no .upgrade-active and removes nothing (mutation.rs:118)"
    } else {
        "removes .upgrade-active"
    }
}

fn answer_reads_as(model: &UpgradeAttempt, before: &State, after: &State) -> &'static str {
    let fix =
        model.fix.is_proposed() && after.answer.is_some_and(|(asker, _)| asker != Asker::Older);
    match after.answer {
        Some((_, Answer::Busy)) if !before.lock_free() => {
            "finds the installation lock held and answers Busy, \"a Ployz installation or upgrade is active\""
        }
        Some((_, Answer::Busy)) if fix && !before.marker => {
            "takes the lock shared, reads a receipt that is not terminal without .upgrade-active and answers Busy (under Broken)"
        }
        Some((_, Answer::Busy)) if fix && before.receipt_terminal() => {
            "takes the lock shared, finds .upgrade-active, ignores the terminal receipt and answers Busy (under Broken)"
        }
        Some((_, Answer::Busy)) if fix && before.receipt.is_none() => {
            "takes the lock shared, finds .upgrade-active, finds no receipt and answers Busy (the fix)"
        }
        Some((_, Answer::Busy)) if fix => {
            "takes the lock shared, finds .upgrade-active, reads a receipt that is not terminal and answers Busy (the fix)"
        }
        Some((_, Answer::Busy)) => {
            "takes the lock shared, finds .upgrade-active and answers Busy, \"a Ployz installation or upgrade is active\""
        }
        Some((_, Answer::Admitted)) if !before.lock_free() => {
            "takes no shared lock and admits while another process holds the lock (under Broken)"
        }
        Some((_, Answer::Admitted)) if before.marker && model.broken.mutation_ignores_marker => {
            "takes the lock shared, does not read .upgrade-active and admits (under Broken)"
        }
        Some((_, Answer::Admitted)) if fix && before.marker && before.receipt.is_none() => {
            "takes the lock shared, finds .upgrade-active, finds no receipt and admits (under Broken)"
        }
        Some((_, Answer::Admitted)) if fix && before.marker && !before.receipt_terminal() => {
            "takes the lock shared, finds .upgrade-active, ignores the receipt and admits (under Broken)"
        }
        Some((_, Answer::Admitted)) if fix && before.marker => {
            "takes the lock shared, finds .upgrade-active, reads a terminal receipt and admits (the fix)"
        }
        Some((_, Answer::Admitted)) if before.marker => {
            "takes the lock shared, does not read .upgrade-active and admits (under Broken)"
        }
        Some((_, Answer::Admitted)) | None => {
            "takes the lock shared, finds no .upgrade-active and admits"
        }
    }
}

/// What a reader's first step did.
fn started(after: &State, caller: Caller) -> String {
    match after.took {
        Some(Branch::NoReceipt) => format!(
            "no receipt, so it returns None{}",
            returns_to(caller, after)
        ),
        Some(Branch::CallerBusy) if caller == Caller::Install => {
            "try_installation answers Busy, and ployzd install prints \"another Ployz installation is active\" (installer::install_at, installer::map_admission_error, `Error::Busy`)".to_owned()
        }
        Some(Branch::CallerBusy) => {
            "try_installation answers Busy; the request returns \"a Ployz installation or upgrade is active\" (local_machine/upgrade.rs:38, machine_api/local.rs:1069-1074)".to_owned()
        }
        took if !caller.settles() => format!(
            "takes the lock; {}",
            reconcile_reads_as(caller, took, after)
        ),
        _ if matches!(
            after.reader,
            Some(Reader {
                at: At::TryTerminal { .. },
                ..
            })
        ) =>
        {
            "settle reads a terminal receipt (installer/upgrade.rs:257); try_installation is next"
                .to_owned()
        }
        _ => "settle reads a receipt that is not terminal; worker_state is next (installer/upgrade.rs:265)".to_owned(),
    }
}

/// What `reconcile_locked` did under the lock, read from the branch it took and where the
/// reader went.
fn reconcile_reads_as(caller: Caller, took: Option<Branch>, after: &State) -> String {
    let at = after.slot(caller).map(|reader| reader.at);
    match took {
        Some(Branch::Interrupts) if after.worker.active() => {
            "reconcile_locked skips worker_state while the unit runs (under Broken) and writes Interrupted (installer/upgrade.rs:384-392); clear_active is next (:393)".to_owned()
        }
        Some(Branch::Interrupts) => {
            "reconcile_locked finds the unit stopped and writes Interrupted (installer/upgrade.rs:381-392); clear_active is next (:393)".to_owned()
        }
        Some(Branch::ReconcileSeesActive) => format!(
            "reconcile_locked finds the unit running and returns (installer/upgrade.rs:381-382){}",
            returns_to(caller, after)
        ),
        _ if at == Some(At::Clear(Site::Terminal)) => {
            "reconcile_locked reads a terminal receipt; clear_active is next (installer/upgrade.rs:377-378)".to_owned()
        }
        _ => "no receipt; the caller checks .upgrade-active next (installer/upgrade.rs:191, :357-362)".to_owned(),
    }
}

/// Where `settle` returned to: Leave's `depart`, or daemon start going on to serve.
fn returns_to(caller: Caller, after: &State) -> &'static str {
    match after.reader {
        Some(Reader {
            at: At::Depart { .. },
            ..
        }) => "; settle returns to Leave's depart",
        None if caller == Caller::DaemonStart && after.daemon == Daemon::Serving => {
            "; daemon start goes on to serve"
        }
        _ => "",
    }
}

fn reader_step_reads_as(
    model: &UpgradeAttempt,
    reader: Reader,
    before: &State,
    after: &State,
) -> String {
    let tail = returns_to(reader.caller, after);
    match reader.at {
        At::QueryWorker { .. } if after.took == Some(Branch::SettleSeesActive) => format!(
            "Reader: worker_state reads the unit active; settle returns the receipt it read (installer/upgrade.rs:265-266){tail}"
        ),
        At::QueryWorker { .. } if before.worker.active() => {
            "Reader: settle skips worker_state (under Broken); try_installation is next (installer/upgrade.rs:268)".to_owned()
        }
        At::QueryWorker { .. } => {
            "Reader: worker_state reads the unit stopped; try_installation is next (installer/upgrade.rs:265, :268)".to_owned()
        }
        At::TryTerminal { .. } if after.took == Some(Branch::SettleBusy) => format!(
            "Reader: try_installation answers Busy; settle returns the terminal receipt it read and leaves .upgrade-active (installer/upgrade.rs:258-263){tail}"
        ),
        At::TryTerminal { .. } => {
            "Reader: try_installation takes the lock; clear_active is next (installer/upgrade.rs:258-259)".to_owned()
        }
        At::TryNonTerminal { .. } if after.took == Some(Branch::SettleBusy) => format!(
            "Reader: try_installation answers Busy; settle returns the receipt it read (installer/upgrade.rs:268-270){tail}"
        ),
        At::TryNonTerminal { seen } if model.broken.settle_reuses_snapshot => format!(
            "Reader: try_installation takes the lock; settle reconciles the {seen:?} it read before the lock, with no re-read (under Broken); {}",
            reconcile_reads_as(reader.caller, after.took, after)
        ),
        At::TryNonTerminal { .. } => format!(
            "Reader: try_installation takes the lock; settle re-reads the receipt (installer/upgrade.rs:268-274); {}",
            reconcile_reads_as(reader.caller, after.took, after)
        ),
        At::Clear(site) => format!(
            "Reader: clear_active {} (installer/upgrade.rs:{}){}",
            clears(after),
            match site {
                Site::Settle => "259",
                Site::Terminal => "378",
                Site::Interrupted => "393",
            },
            if reader.caller.settles() {
                format!("; settle returns and the lock drops{tail}")
            } else {
                String::new()
            }
        ),
        At::Decide => match after.took {
            Some(Branch::NewRequestBusy) => {
                "Reader: request_locked returns Busy and the lock drops (installer/upgrade.rs:186-187 or :191-192)".to_owned()
            }
            Some(Branch::NewRequestGoesOn) => {
                "Reader: request_locked passes :186 and :191 and goes on to resolve its release, which the model leaves out; the lock drops".to_owned()
            }
            Some(Branch::InstallBusy) => {
                "Reader: reconcile_for_install returns Busy, and ployzd install prints \"another Ployz installation is active\" (installer/upgrade.rs:357-369, installer::install_at, installer::map_upgrade_reconciliation, `Error::Busy`)".to_owned()
            }
            _ => {
                "Reader: reconcile_for_install returns Ok; install_locked runs under the lock and returns, and the lock drops (installer::install_at)".to_owned()
            }
        },
        At::Depart { live: true, .. } => {
            "Reader: depart refuses, \"an upgrade is running on this Machine; resend Leave once it finishes\" (local_machine.rs:946-953)".to_owned()
        }
        At::Depart { live: false, .. } => format!(
            "Reader: depart goes on to finish_mutation, whose try_mutation {} (local_machine.rs:959, :355, :333)",
            answer_reads_as(model, before, after)
        ),
    }
}

#[cfg(test)]
mod tests {
    use stateright::Checker;

    use super::*;
    use crate::harness::{self, Narrated, breaks, breaks_where, check, follow, last, took_action};

    type Path = harness::Path<UpgradeAttempt>;

    impl Narrated for UpgradeAttempt {
        fn story(&self, path: &[(State, Option<Action>)]) -> String {
            story(self, path)
        }
    }

    /// The code as it is.
    fn as_is() -> UpgradeAttempt {
        UpgradeAttempt {
            fix: Variant::Shipped,
            broken: Broken::default(),
        }
    }

    /// The fix: the receipt read beside the marker, the marker kept as it is written today.
    fn proposed() -> UpgradeAttempt {
        proposed_without(Fix::default())
    }

    fn as_is_without(broken: Broken) -> UpgradeAttempt {
        UpgradeAttempt { broken, ..as_is() }
    }

    fn proposed_without(fix: Fix) -> UpgradeAttempt {
        UpgradeAttempt {
            fix: Variant::Proposed(fix),
            ..as_is()
        }
    }

    fn steps(path: &Path) -> Vec<Action> {
        path.iter().filter_map(|(_, action)| *action).collect()
    }

    /// The state an action led to, for each action in the path.
    fn after(path: &Path, action: Action) -> Option<State> {
        let at = path.iter().position(|(_, taken)| *taken == Some(action))?;
        path.get(at + 1).map(|(state, _)| *state)
    }

    fn reached(path: &Path, branch: Branch) -> bool {
        path.iter().any(|(state, _)| state.took == Some(branch))
    }

    /// The finding, by its shortest route. `request_locked` writes Failed{Launching} (:235),
    /// and the daemon is killed before `clear_active` (:236). The lock drops with
    /// `.upgrade-active` naming a finished attempt. While the daemon is down, the volume
    /// plugin's `try_mutation` answers Busy on the marker alone (mutation.rs:58-61).
    #[test]
    fn current_code_leaves_a_finished_upgrade_refusing_mutations() {
        let path = breaks(as_is(), BUSY_ONLY_WHILE_LIVE);
        let broken = last(&path);
        assert!(broken.stale_marker());
        assert!(broken.answered(Asker::Plugin, Answer::Busy));
        assert_eq!(broken.daemon, Daemon::Down);
        assert_eq!(
            steps(&path),
            [
                Action::Accept,
                Action::Mark,
                Action::LaunchFails { started: false },
                Action::WriteLaunchFailure,
                Action::DaemonDies { started: false },
                Action::Mutate(Asker::Plugin),
            ]
        );
    }

    /// The worker's route: it writes the terminal receipt (:343) and stops before
    /// `clear_active` (:344), and a daemon RPC answers Busy.
    #[test]
    fn the_worker_leaves_a_finished_upgrade_refusing_mutations() {
        let path = breaks_where(as_is(), BUSY_ONLY_WHILE_LIVE, |path| {
            took_action(path, Action::WorkerFinishes)
                && last(path).answered(Asker::Daemon, Answer::Busy)
        });
        let broken = last(&path);
        assert_eq!(broken.receipt, Some(Outcome::Succeeded));
        assert!(broken.stale_marker());
        assert_eq!(broken.worker, Worker::Stopped);
        assert_eq!(broken.daemon, Daemon::Serving);
        let wrote = path
            .iter()
            .position(|(_, action)| *action == Some(Action::WorkerFinishes))
            .expect("the worker wrote");
        let next = path.get(wrote + 1).and_then(|(_, action)| *action);
        assert!(
            matches!(
                next,
                Some(Action::WorkerDies | Action::ClearFails { removed: false })
            ),
            "the worker stopped before clear_active, got {next:?}"
        );
    }

    /// A settling reader's own route: `reconcile_locked` writes Interrupted (:392) and its
    /// process stops before `clear_active` (:393), or that `clear_active` returns Io.
    #[test]
    fn an_interrupting_reader_leaves_the_same_window() {
        let path = breaks_where(as_is(), BUSY_ONLY_WHILE_LIVE, |path| {
            reached(path, Branch::Interrupts)
        });
        let broken = last(&path);
        assert_eq!(broken.receipt, Some(Outcome::Interrupted));
        assert!(broken.stale_marker());
        assert!(broken.current_answered(Answer::Busy));
    }

    /// The CLI's poll is a resend (`replay`), so it settles, but it can read the terminal
    /// receipt while the worker still holds the lock. `settle`'s `try_installation`
    /// answers Busy and it returns without clearing (:258-263); the poller stops at that
    /// terminal reply (ployz handlers/server/upgrade.rs:710-711). The worker then stops
    /// before its own `clear_active`. A filter on the path cannot pick this schedule: the
    /// checker keeps only the first path to each state, and the last state is the same
    /// with or without the poll.
    #[test]
    fn the_poller_can_stop_before_the_worker_clears() {
        let path = follow(
            as_is(),
            &[
                Action::Accept,
                Action::Mark,
                Action::Launch,
                Action::WorkerLocks,
                Action::WorkerFinishes,
                Action::Start(Caller::Replay),
                Action::ReaderStep,
                Action::WorkerDies,
                Action::Mutate(Asker::Daemon),
            ],
        );
        let polled = after(&path, Action::ReaderStep).expect("the poll settled");
        assert_eq!(polled.took, Some(Branch::SettleBusy));
        assert!(polled.receipt_terminal() && polled.holder == Some(Holder::Worker));
        let broken = last(&path);
        assert!(broken.stale_marker());
        assert!(broken.answered(Asker::Daemon, Answer::Busy));
        let holds = as_is()
            .properties()
            .into_iter()
            .find(|property| property.name == BUSY_ONLY_WHILE_LIVE)
            .expect("the finding's property")
            .condition;
        assert!(
            !holds(&as_is(), &broken),
            "the last state breaks the property"
        );
    }

    /// The window ends: the first path where Leave's own `settle` removes the marker a
    /// stopped writer left, and Leave is then admitted.
    #[test]
    fn a_settling_reader_ends_the_window() {
        let path = breaks(
            as_is(),
            "Leave is admitted after its own settle removed the marker a stopped writer left",
        );
        let admitted = last(&path);
        assert!(admitted.stranded.is_some() && !admitted.marker && admitted.receipt_terminal());
        assert!(
            path.iter()
                .any(|(state, _)| state.cleared == Some(Caller::Leave)),
            "Leave's own settle removed the marker"
        );
    }

    /// The code as it is breaks only the finding's property, and reaches every `sometimes`
    /// the fix does not add.
    #[test]
    fn current_code_breaks_only_the_finding() {
        let model = as_is();
        let checker = check(model);
        println!("as is: {} states", checker.unique_state_count());
        assert!(checker.discovery(BUSY_ONLY_WHILE_LIVE).is_some());
        for property in [
            NO_MUTATION_BESIDE_A_LIVE_WORKER,
            NO_MUTATION_UNDER_THE_LOCK,
            RECEIPT_FINAL,
            INTERRUPTED_ONLY_STOPPED,
            DAEMON_START_SETTLES_FIRST,
            NO_NEW_REFUSAL,
        ] {
            assert!(checker.discovery(property).is_none(), "{property} breaks");
        }
        harness::assert_reached(COVERAGE, &model, &checker);
    }

    /// Without the marker read, a mutation is admitted after the request dropped the lock
    /// and before the worker took it.
    #[test]
    fn the_marker_keeps_mutations_out_of_the_handoff() {
        let path = breaks(
            as_is_without(Broken {
                mutation_ignores_marker: true,
                ..Broken::default()
            }),
            NO_MUTATION_BESIDE_A_LIVE_WORKER,
        );
        let broken = last(&path);
        assert!(broken.lock_free() && broken.marker);
        assert_eq!(broken.worker, Worker::Waiting);
        assert_eq!(broken.receipt, Some(Outcome::Accepted));
    }

    /// Without the shared lock, a mutation is admitted while another process holds the
    /// installation lock.
    #[test]
    fn the_lock_keeps_mutations_out_of_an_installation() {
        let path = breaks(
            as_is_without(Broken {
                mutation_ignores_lock: true,
                ..Broken::default()
            }),
            NO_MUTATION_UNDER_THE_LOCK,
        );
        let broken = last(&path);
        assert!(!broken.lock_free() && !broken.marker);
    }

    /// With the launch before `mark_active`, a daemon killed between them leaves a running
    /// unit, an Accepted receipt and no marker, and the volume plugin admits a mutation.
    #[test]
    fn the_marker_is_written_before_the_launch() {
        let path = breaks(
            as_is_without(Broken {
                launches_before_marking: true,
                ..Broken::default()
            }),
            NO_MUTATION_BESIDE_A_LIVE_WORKER,
        );
        let broken = last(&path);
        assert!(!broken.marker && broken.lock_free());
        assert_eq!(broken.worker, Worker::Waiting);
        assert_eq!(broken.receipt, Some(Outcome::Accepted));
    }

    /// Without the worker's `is_terminal` check (:308), a unit that started after
    /// `launch_worker` timed out writes Running over Failed{Launching}.
    #[test]
    fn the_worker_refuses_a_finished_attempt() {
        let path = breaks(
            as_is_without(Broken {
                worker_ignores_terminal: true,
                ..Broken::default()
            }),
            RECEIPT_FINAL,
        );
        assert!(took_action(&path, Action::LaunchFails { started: true }));
        let broken = last(&path);
        assert_eq!(broken.first_terminal, Some(Outcome::Failed));
        assert_eq!(broken.receipt, Some(Outcome::Running));
    }

    /// Without `settle`'s re-read under the lock (:273), a reader that read Accepted or
    /// Running before the worker finished writes Interrupted over its terminal receipt.
    #[test]
    fn settle_rereads_the_receipt_under_the_lock() {
        let path = breaks(
            as_is_without(Broken {
                settle_reuses_snapshot: true,
                ..Broken::default()
            }),
            RECEIPT_FINAL,
        );
        let broken = last(&path);
        assert_eq!(broken.receipt, Some(Outcome::Interrupted));
        assert!(
            broken
                .first_terminal
                .is_some_and(|first| first != Outcome::Interrupted)
        );
    }

    /// Without `reconcile_locked`'s `worker_state` check (:381), a new request or
    /// `ployzd install` writes Interrupted while the worker's unit waits for the lock.
    #[test]
    fn reconcile_locked_needs_its_worker_check() {
        let path = breaks(
            as_is_without(Broken {
                reconcile_skips_worker_state: true,
                ..Broken::default()
            }),
            INTERRUPTED_ONLY_STOPPED,
        );
        let broken = last(&path);
        assert_eq!(broken.worker, Worker::Waiting);
        let writer = match broken.holder {
            Some(Holder::Install) => broken.install,
            _ => broken.reader,
        };
        assert!(writer.is_some_and(|reader| !reader.caller.settles()));
    }

    /// `settle`'s own `worker_state` check (:265) runs before it takes the lock (:268), so
    /// a unit can start between the two. `reconcile_locked`'s check (:381), under the lock,
    /// keeps the safety properties. Without `settle`'s check, `INTERRUPTED_ONLY_STOPPED` and
    /// `RECEIPT_FINAL` still hold, and only its own `COVERAGE` row goes unreached. Without `reconcile_locked`'s, a settle caller writes
    /// Interrupted beside a unit that started after `settle`'s `worker_state` read it
    /// stopped.
    #[test]
    fn settle_relies_on_the_worker_check_under_the_lock() {
        let model = as_is_without(Broken {
            settle_skips_worker_state: true,
            ..Broken::default()
        });
        let checker = check(model);
        for property in [INTERRUPTED_ONLY_STOPPED, RECEIPT_FINAL] {
            assert!(checker.discovery(property).is_none(), "{property} breaks");
        }
        let unreached: Vec<_> = COVERAGE
            .iter()
            .filter(|row| (row.on)(&model) && checker.discovery(row.name).is_none())
            .map(|row| row.name)
            .collect();
        assert_eq!(
            unreached,
            ["settle returns a running worker's receipt as it stands"]
        );
        let path = breaks_where(
            as_is_without(Broken {
                reconcile_skips_worker_state: true,
                ..Broken::default()
            }),
            INTERRUPTED_ONLY_STOPPED,
            |path| {
                let broken = last(path);
                broken.holder == Some(Holder::Reader)
                    && broken.reader.is_some_and(|reader| reader.caller.settles())
            },
        );
        assert_eq!(last(&path).worker, Worker::Waiting);
        let read_stopped = path.windows(2).any(|pair| {
            let [(before, action), (after, _)] = pair else {
                return false;
            };
            *action == Some(Action::ReaderStep)
                && matches!(
                    before.reader,
                    Some(Reader {
                        at: At::QueryWorker { .. },
                        ..
                    })
                )
                && !before.worker.active()
                && matches!(
                    after.reader,
                    Some(Reader {
                        at: At::TryNonTerminal { .. },
                        ..
                    })
                )
        });
        assert!(read_stopped, "settle's worker_state read the unit stopped");
    }

    /// Daemon start's `settle` clears the marker only if it wins `try_installation`
    /// (:258-260). When `ployzd install` holds the lock (`installer::install_at`), `settle`
    /// returns the terminal receipt as Ok (:260, :263), `reconcile` drops it (:349-351) and
    /// the daemon serves (daemon.rs:187). Install then stops before its `clear_active`, and a
    /// daemon RPC answers Busy on the marker the killed daemon left.
    #[test]
    fn daemon_start_loses_the_lock_to_ployzd_install() {
        let path = breaks_where(
            as_is(),
            "a daemon RPC refuses on the marker a killed daemon left after daemon start lost try_installation",
            |path| start_lost_to(path, Holder::Install),
        );
        assert!(last(&path).daemon == Daemon::Serving && last(&path).stale_marker());
    }

    /// The same with a worker that `systemd-run` started though `launch_worker` timed out
    /// (:440-447): it finds Failed{Launching} terminal and holds the lock from
    /// `lock_installation` (:306) until it returns NotActive (:309).
    #[test]
    fn daemon_start_loses_the_lock_to_a_late_worker() {
        let path = breaks_where(
            as_is(),
            "a daemon RPC refuses on the marker a killed daemon left after daemon start lost try_installation",
            |path| start_lost_to(path, Holder::Worker),
        );
        assert!(took_action(&path, Action::LaunchFails { started: true }));
        assert!(took_action(&path, Action::WorkerRefuses));
    }

    /// Daemon start's `settle` found the lock held by `holder` and returned without
    /// clearing.
    fn start_lost_to(path: &Path, holder: Holder) -> bool {
        path.windows(2).any(|pair| {
            let [(before, action), (after, _)] = pair else {
                return false;
            };
            *action == Some(Action::ReaderStep)
                && before
                    .reader
                    .is_some_and(|reader| reader.caller == Caller::DaemonStart)
                && before.holder == Some(holder)
                && after.took == Some(Branch::SettleBusy)
        })
    }

    /// Each clearing row's witness shows the caller its name gives.
    #[test]
    fn each_clearing_row_is_its_callers() {
        let checker = check(as_is());
        for (caller, row) in [
            (
                Caller::Inspect,
                "Inspect's settle removes the marker a stopped writer left",
            ),
            (
                Caller::Replay,
                "a resend's settle removes the marker a stopped writer left",
            ),
            (
                Caller::Leave,
                "Leave's settle removes the marker a stopped writer left",
            ),
            (
                Caller::DaemonStart,
                "daemon start's settle removes the marker a stopped writer left",
            ),
            (
                Caller::NewRequest,
                "a new request's reconcile_locked removes the marker a stopped writer left",
            ),
            (
                Caller::Install,
                "ployzd install's reconcile_locked removes the marker a stopped writer left",
            ),
        ] {
            let witness = checker.discovery(row).expect("the row is reached");
            assert_eq!(witness.last_state().cleared, Some(caller), "{row}");
        }
    }

    /// If the daemon served again after a kill without daemon start's `reconcile`
    /// (daemon.rs:184-186), a daemon RPC would answer Busy on the marker the killed
    /// request left, with no start that lost the lock.
    #[test]
    fn daemon_start_settles_before_serving() {
        let path = breaks(
            as_is_without(Broken {
                serves_before_reconcile: true,
                ..Broken::default()
            }),
            DAEMON_START_SETTLES_FIRST,
        );
        let broken = last(&path);
        assert!(broken.answered(Asker::Daemon, Answer::Busy));
        assert!(took_action(&path, Action::DaemonDies { started: false }));
        assert!(!took_action(&path, Action::Start(Caller::DaemonStart)));
    }

    /// The fix holds, and every `sometimes` it reaches is reached.
    #[test]
    fn the_fix_answers_from_the_receipt() {
        let checker = check(proposed());
        println!("fixed: {} states", checker.unique_state_count());
        checker.assert_properties();
    }

    /// Without the receipt, the fix's admission is the marker alone, as the code as it is,
    /// and the finding comes back.
    #[test]
    fn the_fix_needs_the_receipt() {
        let path = breaks(
            proposed_without(Fix {
                ignores_receipt: true,
                ..Fix::default()
            }),
            BUSY_ONLY_WHILE_LIVE,
        );
        let broken = last(&path);
        assert!(broken.current_answered(Answer::Busy) && broken.stale_marker());
    }

    /// Read without the marker, the receipt refuses where the code as it is admits: a
    /// daemon killed between the Accepted write (:227) and `mark_active` (:228) leaves
    /// Accepted with no marker.
    #[test]
    fn the_fix_reads_the_receipt_only_beside_the_marker() {
        let path = breaks(
            proposed_without(Fix {
                skips_marker_gate: true,
                ..Fix::default()
            }),
            NO_NEW_REFUSAL,
        );
        let broken = last(&path);
        assert!(broken.current_answered(Answer::Busy) && broken.lock_free() && !broken.marker);
        assert_eq!(broken.receipt, Some(Outcome::Accepted));
    }

    /// Without the marker, the fix's own admission lets a mutation in between the request
    /// and the worker.
    #[test]
    fn the_fix_still_writes_the_marker() {
        let path = breaks(
            proposed_without(Fix {
                writes_no_marker: true,
                ..Fix::default()
            }),
            NO_MUTATION_BESIDE_A_LIVE_WORKER,
        );
        let broken = last(&path);
        assert!(broken.current_answered(Answer::Admitted));
        assert_eq!(broken.worker, Worker::Waiting);
        assert!(!broken.marker);
    }

    /// Under the fix, `.upgrade-active` with no receipt still refuses, as `try_mutation`
    /// and `reconcile_for_install` (:357-362) do today.
    #[test]
    fn the_fix_refuses_a_marker_with_no_receipt() {
        let path = breaks(
            proposed(),
            "try_mutation refuses on .upgrade-active with no receipt",
        );
        let refused = last(&path);
        assert!(refused.marker && refused.receipt.is_none());
        assert!(refused.current_answered(Answer::Busy));
    }

    /// Under the fix, a release before it still refuses on the marker a stopped writer
    /// left.
    #[test]
    fn the_fix_leaves_older_readers_their_window() {
        let path = breaks(
            proposed(),
            "an older reader refuses on the marker of a finished attempt under the fix",
        );
        let refused = last(&path);
        assert!(refused.stranded.is_some() && refused.stale_marker());
        assert!(refused.answered(Asker::Older, Answer::Busy));
    }

    /// Under the fix, an attempt whose worker stopped before a terminal write still refuses
    /// beside its marker, as on the code as it is, until a reader writes Interrupted.
    #[test]
    fn the_fix_keeps_the_dead_attempt_window() {
        let path = breaks(
            proposed(),
            "the fix refuses on the marker of an attempt whose worker stopped before a terminal write",
        );
        let refused = last(&path);
        assert!(refused.receipt_live() && refused.marker && refused.lock_free());
        assert_eq!(refused.worker, Worker::Stopped);
        assert!(took_action(&path, Action::WorkerDies));
    }
}
