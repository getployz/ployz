//! What Docker's events say of each container's runs, and the settlement that
//! records each exit once.
//!
//! A run's `kill`, `stop` and `oom` events can come before or after the `die`
//! of the run they belong to, so the ledger keeps each run's evidence until
//! the inspect that follows its exit settles it. A crash then lands in the
//! crash `Outbox` (which the crash sender posts to the Collector), and every
//! exit in the [`Settlements`] record, through [`Settler::settle`].

use std::{
    collections::{HashMap, HashSet},
    io,
    ops::ControlFlow,
    sync::Arc,
};

use bollard::models::ContainerState;
use ployz_core::{
    ContainerId,
    metrics::{MarkerKind, MetricMarker},
};

use super::{
    clock::rfc3339_nanos,
    layout::StoreRoot,
    settled::{Key, Settlement, Settlements, Unfinished},
    ship::{Outbox, crash_fits},
    signal::{Signal, is_stop_signal},
};

/// An event that can arrive after the run it belongs to has exited.
pub(super) enum Late {
    /// Docker was asked to send the run this signal.
    Kill(Signal),
    /// Docker's stop API stopped the run.
    Stop,
    /// The kernel's OOM killer killed a process in the run.
    Oom,
}

/// What Docker's events said of a container's current run.
#[derive(Debug)]
enum Run {
    Live(Evidence),
    /// It died and has not started again. Its evidence went with its exit.
    Exited,
}

/// The evidence Docker's events gave about one run of a container.
#[derive(Debug, Default)]
struct Evidence {
    oom: bool,
    /// The signals Docker was asked to send it, as `docker stop` does.
    kills: Vec<Signal>,
    /// Docker's stop API stopped it, whatever signal it sent. A bare
    /// `docker kill` sends no `stop` event.
    stop_requested: bool,
}

impl Evidence {
    /// Whether `late` belongs to this live run. Docker's stop API signals a
    /// run before it sends `stop`, so a live run with no `kill` is not the
    /// one stopped.
    fn awaits(&self, late: &Late) -> bool {
        !matches!(late, Late::Stop) || !self.kills.is_empty()
    }

    fn apply(&mut self, late: Late) {
        match late {
            Late::Kill(signal) => self.kills.push(signal),
            Late::Stop => self.stop_requested = true,
            Late::Oom => self.oom = true,
        }
    }
}

/// An exit and its run's evidence, waiting on the inspect that follows it.
#[derive(Debug)]
struct Exit {
    marker: MetricMarker,
    evidence: Evidence,
    /// The harvester saw nothing of the run before its exit, as after a
    /// restart of `ployz observe` while Docker stopped the container.
    unseen: bool,
    /// The stream had resumed from an earlier event, so it delivered the
    /// `kill` of any `docker stop` that ended the run.
    resumed: bool,
    stop_signal: Option<Signal>,
}

impl Exit {
    /// The exit's marker, with the OOM kill taken from `state` only when
    /// `state` describes this run: a later run's inspect says nothing of it.
    fn settle(self, state: Option<&ContainerState>, stop_signal: Option<Signal>) -> MetricMarker {
        let Exit {
            mut marker,
            evidence:
                Evidence {
                    oom,
                    kills,
                    stop_requested,
                },
            unseen,
            resumed,
            stop_signal: stop_signal_at_exit,
        } = self;
        let died = marker.ts;
        let stop_signal = stop_signal.or(stop_signal_at_exit);
        let is_stop = |signal: Signal| is_stop_signal(signal, stop_signal);
        if let MarkerKind::Exit {
            exit_code,
            oom: killed_by_oom,
            stopped,
        } = &mut marker.kind
        {
            *killed_by_oom = oom
                || state
                    .is_some_and(|state| ends_run(state, died) && state.oom_killed == Some(true));
            // A stop signal's exit with no run to say who sent it is taken
            // as `docker stop`, unless the stream resumed early enough to
            // deliver that stop's `kill`.
            *stopped = stop_requested
                || kills.into_iter().any(is_stop)
                || (unseen
                    && !resumed
                    && !*killed_by_oom
                    && exit_code
                        .and_then(|code| Signal::from_number(code.checked_sub(128)?))
                        .is_some_and(is_stop));
        }
        marker
    }
}

pub(super) struct ExitLedger {
    /// What the events said of each container's current run.
    runs: HashMap<ContainerId, Run>,
    /// Exits that wait on the inspect that follows them: Docker can send
    /// `die` before `oom`, or drop the `oom` event.
    exits: HashMap<ContainerId, Vec<Exit>>,
    /// The `Config.StopSignal` the last inspect gave, for an exit whose own
    /// inspect finds the container gone.
    stop_signals: HashMap<ContainerId, Signal>,
    pub(super) settler: Settler,
}

impl ExitLedger {
    pub(super) fn new(store: &StoreRoot, crashes: Arc<Outbox>) -> Self {
        Self {
            runs: HashMap::new(),
            exits: HashMap::new(),
            stop_signals: HashMap::new(),
            settler: Settler::open(store, crashes),
        }
    }

    pub(super) fn waits_on(&self, id: &ContainerId) -> bool {
        self.exits.contains_key(id)
    }

    pub(super) fn note_stop_signal(&mut self, id: ContainerId, signal: Signal) {
        self.stop_signals.insert(id, signal);
    }

    /// Drops the evidence of containers Docker no longer lists. The exits
    /// still queued stay: they wait on an inspect that can still answer.
    pub(super) fn keep_present(&mut self, present: &HashSet<ContainerId>) {
        self.runs.retain(|id, _| present.contains(id));
        self.stop_signals.retain(|id, _| present.contains(id));
    }

    pub(super) fn forget(&mut self, id: &ContainerId) {
        self.runs.remove(id);
        self.stop_signals.remove(id);
    }

    /// Gives `late` to the live run of `id`, else to its last queued exit:
    /// Docker can send `stop` or `oom` after the `die` of the run it stopped
    /// or killed, even after a later run started. A `kill` after the exit
    /// belongs to no exit, so it is dropped.
    pub(super) fn late(&mut self, id: ContainerId, late: Late) {
        let run = self
            .runs
            .entry(id)
            .or_insert_with(|| Run::Live(Evidence::default()));
        match (run, late) {
            (Run::Exited, Late::Kill(_)) => {}
            (Run::Live(evidence), late) if evidence.awaits(&late) => evidence.apply(late),
            (_, late) => {
                if let Some(exit) = self.exits.get_mut(&id).and_then(|exits| exits.last_mut()) {
                    exit.evidence.apply(late);
                }
            }
        }
    }

    /// Queues the exit of the live run with what the events said of it.
    /// `resumed` says the open stream resumed from an earlier event.
    pub(super) fn died(&mut self, marker: MetricMarker, resumed: bool) {
        let id = marker.container_id;
        let (unseen, evidence) = match self.runs.insert(id, Run::Exited) {
            Some(Run::Live(evidence)) => (false, evidence),
            Some(Run::Exited) => (false, Evidence::default()),
            None => (true, Evidence::default()),
        };
        let exit = Exit {
            marker,
            evidence,
            unseen,
            resumed,
            stop_signal: self.stop_signals.get(&id).copied(),
        };
        self.exits.entry(id).or_default().push(exit);
    }

    pub(super) fn started(&mut self, id: ContainerId) {
        self.runs.insert(id, Run::Live(Evidence::default()));
    }

    /// Settles the exits waiting on `id` with what Docker says now, from
    /// `state` and `stop_signal` of its inspect. An inspect that Docker did
    /// not answer leaves the events' evidence alone.
    pub(super) fn settle_exits(
        &mut self,
        id: &ContainerId,
        state: Option<&ContainerState>,
        stop_signal: Option<Signal>,
    ) {
        let Some(exits) = self.exits.remove(id) else {
            return;
        };
        for exit in exits {
            let marker = exit.settle(state, stop_signal);
            self.settler.settle(marker);
        }
    }
}

/// Records each exit and OOM kill once, whatever order a failed write, a
/// stopped harvester or a resumed stream delivers them in.
pub(super) struct Settler {
    settlements: Settlements,
    crashes: Arc<Outbox>,
}

impl Settler {
    fn open(store: &StoreRoot, crashes: Arc<Outbox>) -> Self {
        Self {
            settlements: Settlements::open(store),
            crashes,
        }
    }

    pub(super) fn count(&self) -> io::Result<usize> {
        self.settlements.count()
    }

    pub(super) fn forget_before(&self, before: i64) -> io::Result<()> {
        self.settlements.forget_before(before)
    }

    /// Records an exit or OOM kill once: a forgotten container's `die` or
    /// `oom` can come again in a resumed stream, and its exit settled again
    /// from less evidence.
    ///
    /// One settlement runs these steps in this order: reserve the crash's
    /// outbox seq, save the seq, push the crash under it, finish the
    /// settlement. Each step is saved before the next begins, so an attempt a
    /// failed write or a stop cuts short resumes from what [`Unfinished`]
    /// says it reached: the crash is pushed under the one seq it reserved, so
    /// pushing again rewrites the same entry and never queues a second.
    /// Finishing comes last because only a finished settlement stops an event
    /// delivered again from settling again.
    pub(super) fn settle(&mut self, marker: MetricMarker) {
        let Some(key) = Key::of(&marker) else {
            return;
        };
        match self.settlements.get(&key) {
            Ok(Settlement::New) => {
                // A crash's entry is checked before anything is saved or reserved for it, and
                // a settlement's save checks it at any seq it can take, so a settlement that
                // started fits each later step.
                if marker.is_crash() && !crash_fits(&marker) {
                    self.refuse(&key);
                    return;
                }
                let unfinished = Unfinished { marker, seq: None };
                match self.settlements.save(&key, &unfinished) {
                    Err(error) if error.kind() == io::ErrorKind::FileTooLarge => {
                        self.refuse(&key);
                        return;
                    }
                    Err(error) => {
                        tracing::warn!(container = %key.container(), %error, "cannot record an exit before settling it");
                        return;
                    }
                    Ok(()) => {}
                }
                self.advance(&key, unfinished);
            }
            Ok(Settlement::Unfinished(unfinished)) => self.advance(&key, *unfinished),
            Ok(Settlement::Finished) => {}
            Err(error) => {
                tracing::warn!(container = %key.container(), %error, "cannot read whether an exit was settled");
            }
        }
    }

    /// Drops an exit too large for the observe store, counted with the crashes dropped.
    fn refuse(&self, key: &Key) {
        tracing::warn!(container = %key.container(), "an exit is too large for the observe store; dropping it");
        if let Err(error) = self.crashes.refuse() {
            tracing::warn!(%error, "cannot count a dropped exit");
        }
    }

    /// Runs the steps of [`Settler::settle`] a settlement has not reached.
    fn advance(&mut self, key: &Key, unfinished: Unfinished) {
        if unfinished.marker.is_crash() && self.queue_crash(key, unfinished).is_break() {
            return;
        }
        if let Err(error) = self.settlements.finish(key) {
            tracing::warn!(container = %key.container(), %error, "cannot record that an exit was settled");
        }
    }

    fn queue_crash(&mut self, key: &Key, mut unfinished: Unfinished) -> ControlFlow<()> {
        let id = key.container();
        let seq = match unfinished.seq {
            Some(seq) => seq,
            None => {
                let seq = match self.crashes.reserve(unfinished.marker.ts) {
                    Ok(seq) => seq,
                    Err(error) => {
                        tracing::warn!(container = %id, %error, "cannot queue a crash for Cloud");
                        return ControlFlow::Break(());
                    }
                };
                unfinished.seq = Some(seq);
                if let Err(error) = self.settlements.save(key, &unfinished) {
                    tracing::warn!(container = %id, %error, "cannot record a crash before queuing it");
                    return ControlFlow::Break(());
                }
                seq
            }
        };
        if let Err(error) = self.crashes.push_at(seq, unfinished.marker) {
            tracing::warn!(container = %id, %error, "cannot queue a crash for Cloud");
            return ControlFlow::Break(());
        }
        ControlFlow::Continue(())
    }

    /// Finishes the settlements a failed write or a stopped harvester left
    /// unfinished, and gives up those of
    /// events older than `give_up_before` that no stream can repeat. A crash
    /// the outbox has not queued is never given up: only a real crash can
    /// leave one, and it waits for the outbox to take it.
    pub(super) fn resume_unfinished(&mut self, give_up_before: i64) {
        let unfinished = match self.settlements.unfinished() {
            Ok(unfinished) => unfinished,
            Err(error) => {
                tracing::warn!(%error, "cannot list unfinished settlements");
                return;
            }
        };
        let mut kept = Vec::new();
        for (key, entry) in unfinished {
            if key.ts() >= give_up_before || self.crash_unqueued(&entry) {
                kept.push((key, entry));
                continue;
            }
            tracing::warn!(container = %key.container(), "giving up an exit that could not be settled for a day");
            if let Err(error) = self.settlements.forget(&key) {
                tracing::warn!(container = %key.container(), %error, "cannot give up an exit settlement");
            }
        }
        let reserved: Vec<u64> = kept.iter().filter_map(|(_, entry)| entry.seq).collect();
        if let Err(error) = self.crashes.release_reserved(&reserved) {
            tracing::warn!(%error, "cannot release crash seqs nothing will queue");
        }
        for (key, entry) in kept {
            self.advance(&key, entry);
        }
    }

    fn crash_unqueued(&self, entry: &Unfinished) -> bool {
        entry.marker.is_crash()
            && entry
                .seq
                .is_none_or(|seq| self.crashes.is_reserved(seq).unwrap_or(true))
    }
}

/// Whether `state` describes the run that died at `died`. Docker sets
/// `FinishedAt` before it sends `die`, and starts any later run after it.
fn ends_run(state: &ContainerState, died: i64) -> bool {
    let at = |time: &Option<String>| time.as_deref().and_then(rfc3339_nanos);
    at(&state.started_at)
        .zip(at(&state.finished_at))
        .is_some_and(|(started, finished)| started <= finished && finished <= died)
}
