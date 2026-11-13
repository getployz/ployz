//! What Docker's events say of each container's runs, and the settlement that
//! records each exit once.
//!
//! A run's `kill`, `stop` and `oom` events can come before or after the `die`
//! of the run they belong to, so the ledger keeps each run's evidence until
//! the inspect that follows its exit settles it. An exit then lands in
//! `exits.jsonl`, the metric segment, the crash `Outbox` and the
//! [`Settlements`] record through [`Settler::settle`].

use std::{
    collections::{HashMap, HashSet},
    io,
    ops::ControlFlow,
    sync::Arc,
};

use bollard::models::ContainerState;
use memchr::memmem::Finder;
use ployz_core::{
    ContainerId,
    metrics::{MarkerKind, MetricMarker, MetricRange},
};

use super::{
    clock::rfc3339_nanos,
    layout::{EXITS_FILE, RunExit, StoreRoot, append_line},
    metrics::segment::{self, Prefilter, Row},
    settled::{Key, Settlement, Settlements, Unfinished},
    ship::Outbox,
    signal::{Signal, is_stop_signal},
};

/// The raw segments one maintenance tick may read to resume settlements.
const SETTLEMENT_SCANS_PER_TICK: usize = 8;

/// An event that can arrive after the run it belongs to has exited.
pub(super) enum Late {
    /// Docker was asked to send the run this signal.
    Kill(Signal),
    /// Docker's stop API stopped the run.
    Stop,
    /// The kernel's OOM killer killed a process in the run.
    Oom,
}

/// The evidence Docker's events gave about one run of a container.
#[derive(Debug, Default)]
struct Run {
    oom: bool,
    /// The signals Docker was asked to send it, as `docker stop` does.
    kills: Vec<Signal>,
    /// Docker's stop API stopped it, whatever signal it sent. A bare
    /// `docker kill` sends no `stop` event.
    stop_requested: bool,
    exited: bool,
}

impl Run {
    /// Whether `late` belongs to this live run. Otherwise it belongs to the
    /// last exit still queued: Docker can send `stop` or `oom` after the
    /// `die` of the run it stopped or killed, even after a later run started.
    /// Its stop API signals a run before it sends `stop`, so a live run with
    /// no `kill` is not the one stopped. A `kill` after the exit belongs to
    /// no exit, so it stays with the run it came after.
    fn awaits(&self, late: &Late) -> bool {
        match late {
            Late::Kill(_) => true,
            Late::Stop => !self.exited && !self.kills.is_empty(),
            Late::Oom => !self.exited,
        }
    }

    fn apply(&mut self, late: Late) {
        match late {
            Late::Kill(signal) => {
                if !self.exited {
                    self.kills.push(signal);
                }
            }
            Late::Stop => self.stop_requested = true,
            Late::Oom => self.oom = true,
        }
    }
}

/// An exit and its run's evidence, waiting on the inspect that follows it.
#[derive(Debug)]
struct Exit {
    marker: MetricMarker,
    run: Run,
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
            run:
                Run {
                    oom,
                    kills,
                    stop_requested,
                    ..
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
    store: StoreRoot,
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
            store: store.clone(),
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

    /// Gives `late` to the live run of `id`, else to its last queued exit.
    pub(super) fn late(&mut self, id: ContainerId, late: Late) {
        let run = self.runs.entry(id).or_default();
        if run.awaits(&late) {
            run.apply(late);
        } else if let Some(exit) = self.exits.get_mut(&id).and_then(|exits| exits.last_mut()) {
            exit.run.apply(late);
        }
    }

    /// Queues the exit of the live run with what the events said of it.
    /// `resumed` says the open stream resumed from an earlier event.
    pub(super) fn died(&mut self, marker: MetricMarker, resumed: bool) {
        let id = marker.container_id;
        let unseen = !self.runs.contains_key(&id);
        let run = self.runs.entry(id).or_default();
        let run = std::mem::replace(
            run,
            Run {
                exited: true,
                ..Run::default()
            },
        );
        let exit = Exit {
            marker,
            run,
            unseen,
            resumed,
            stop_signal: self.stop_signals.get(&id).copied(),
        };
        self.exits.entry(id).or_default().push(exit);
    }

    /// Starts a run, and says whether the one before it had exited.
    pub(super) fn started(&mut self, id: ContainerId) -> bool {
        self.runs
            .insert(id, Run::default())
            .is_some_and(|run| run.exited)
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
            self.keep_exit(&marker);
            self.settler.settle(marker);
        }
    }

    /// Keeps the exit beside the container's logs, which hold every run.
    /// A container the store does not hold has no directory to keep it in.
    /// It stays outside [`Settler::settle`], which a settled exit makes a
    /// no-op: an exit settled again once the OOM kill is learned rewrites it.
    fn keep_exit(&self, marker: &MetricMarker) {
        let MarkerKind::Exit { exit_code, oom, .. } = marker.kind else {
            return;
        };
        let id = marker.container_id;
        let exit = RunExit {
            ts: marker.ts,
            exit_code,
            oom_killed: oom,
        };
        if let Err(error) = append_line(&self.store.container(&id).join(EXITS_FILE), &exit)
            && error.kind() != io::ErrorKind::NotFound
        {
            tracing::warn!(container = %id, %error, "cannot keep a run's exit for history");
        }
    }
}

/// Records each exit and OOM kill once, whatever order a failed write, a
/// stopped harvester or a resumed stream delivers them in.
pub(super) struct Settler {
    store: StoreRoot,
    settlements: Settlements,
    crashes: Arc<Outbox>,
    /// Whether this process knows a settlement's marker is in the raw
    /// segment, so a resume reads the segment at most once.
    marker_stored: HashMap<Key, bool>,
}

impl Settler {
    fn open(store: &StoreRoot, crashes: Arc<Outbox>) -> Self {
        Self {
            store: store.clone(),
            settlements: Settlements::open(store),
            crashes,
            marker_stored: HashMap::new(),
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
    /// One settlement runs these steps in this order: append the marker to
    /// the raw segment, save that it was appended, reserve the crash's outbox
    /// seq, save the seq, push the crash under it, finish the settlement.
    /// Each step is saved before the next begins, so an attempt a failed
    /// write or a stop cuts short resumes from what [`Unfinished`] says it
    /// reached: the marker is appended once, and the crash is pushed under
    /// the one seq it reserved, so pushing again rewrites the same entry and
    /// never queues a second. Finishing comes last because only a finished
    /// settlement stops an event delivered again from settling again. A
    /// restart has no settlement: it is recorded only after an exit the open
    /// stream saw, which a resumed stream cannot repeat.
    pub(super) fn settle(&mut self, marker: MetricMarker) {
        let Some(key) = Key::of(&marker) else {
            if let Err(error) = segment::append(&self.store, &[Row::Marker(marker.clone())]) {
                tracing::warn!(container = %marker.container_id, %error, "cannot append a metric marker");
            }
            return;
        };
        match self.settlements.get(&key) {
            Ok(Settlement::New) => {
                let unfinished = Unfinished {
                    marker,
                    appended: false,
                    seq: None,
                };
                if let Err(error) = self.settlements.save(&key, &unfinished) {
                    tracing::warn!(container = %key.container(), %error, "cannot record an exit before settling it");
                    return;
                }
                self.advance(&key, unfinished, false);
            }
            Ok(Settlement::Unfinished(unfinished)) => self.advance(&key, *unfinished, true),
            Ok(Settlement::Finished) => {}
            Err(error) => {
                tracing::warn!(container = %key.container(), %error, "cannot read whether an exit was settled");
            }
        }
    }

    /// Runs the steps of [`Settler::settle`] a settlement has not reached.
    /// One `resumed` from an earlier attempt may have stored its marker
    /// before it could say so.
    fn advance(&mut self, key: &Key, mut unfinished: Unfinished, resumed: bool) {
        if !unfinished.appended && self.append_marker(key, &mut unfinished, resumed).is_break() {
            return;
        }
        if unfinished.marker.is_crash() && self.queue_crash(key, unfinished).is_break() {
            return;
        }
        if let Err(error) = self.settlements.finish(key) {
            tracing::warn!(container = %key.container(), %error, "cannot record that an exit was settled");
            return;
        }
        self.marker_stored.remove(key);
    }

    fn append_marker(
        &mut self,
        key: &Key,
        unfinished: &mut Unfinished,
        resumed: bool,
    ) -> ControlFlow<()> {
        let id = key.container();
        let stored = match self.marker_stored.get(key) {
            Some(stored) => *stored,
            None if resumed => match marker_stored(&self.store, &unfinished.marker) {
                Ok(stored) => stored,
                Err(error) => {
                    tracing::warn!(container = %id, %error, "cannot read whether a metric marker was appended");
                    return ControlFlow::Break(());
                }
            },
            None => false,
        };
        self.marker_stored.insert(*key, stored);
        if !stored {
            if let Err(error) =
                segment::append(&self.store, &[Row::Marker(unfinished.marker.clone())])
            {
                tracing::warn!(container = %id, %error, "cannot append a metric marker");
                return ControlFlow::Break(());
            }
            self.marker_stored.insert(*key, true);
        }
        unfinished.appended = true;
        if let Err(error) = self.settlements.save(key, unfinished) {
            tracing::warn!(container = %id, %error, "cannot record that a metric marker was appended");
        }
        ControlFlow::Continue(())
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
    /// unfinished, reading at most a few raw segments, and gives up those of
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
            match self.settlements.forget(&key) {
                Ok(()) => {
                    self.marker_stored.remove(&key);
                }
                Err(error) => {
                    tracing::warn!(container = %key.container(), %error, "cannot give up an exit settlement");
                }
            }
        }
        let reserved: Vec<u64> = kept.iter().filter_map(|(_, entry)| entry.seq).collect();
        if let Err(error) = self.crashes.release_reserved(&reserved) {
            tracing::warn!(%error, "cannot release crash seqs nothing will queue");
        }
        let mut scans = SETTLEMENT_SCANS_PER_TICK;
        for (key, entry) in kept {
            if !entry.appended && !self.marker_stored.contains_key(&key) {
                if scans == 0 {
                    continue;
                }
                scans -= 1;
            }
            self.advance(&key, entry, true);
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

/// Whether a raw segment holds `marker`.
fn marker_stored(store: &StoreRoot, marker: &MetricMarker) -> io::Result<bool> {
    segment::find(
        &store.raw_metrics(),
        MetricRange {
            since_nanos: marker.ts,
            until_nanos: marker.ts.saturating_add(1),
        },
        &Prefilter::Contains(Finder::new(marker.container_id.to_string().as_bytes()).into_owned()),
        |row: &Row| matches!(row, Row::Marker(found) if found == marker),
    )
}
