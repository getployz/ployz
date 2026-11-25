//! The receive task: one stream into a slot's `fs`, from the writer Machine or, for a
//! seed, from a local snapshot.

use std::{
    collections::BTreeMap,
    future::Future,
    process::Stdio,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use crate::faults::{Identity, Point, Want};
use futures_util::{FutureExt as _, StreamExt as _};
use ployz_core::{
    CopyTransition, ManagementAddress, SendSource, SendStream, SnapshotGuid, SnapshotName,
    VolumeGen, error_chain::inline,
};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    process::{Child, Command},
    time::Instant,
};
use tokio_util::sync::{CancellationToken, DropGuard};

use super::{PluginError, VolumeName, VolumeStorage, storage::HeldMutation};

/// The Volume and target a pull of `stream` lands, for the lane at `gen`.
pub(super) fn pulling(stream: &SendStream, r#gen: VolumeGen) -> Identity<'_> {
    let (SendSource::Full { target }
    | SendSource::Incremental { target, .. }
    | SendSource::Resume { target, .. }) = &stream.source;
    Identity {
        volume: &stream.name,
        want: Want::Mirror { target },
        r#gen,
    }
}

/// A receive's `kill-daemon` fault fires once this much of the stream is in, so ZFS holds a
/// partial receive to resume.
const KILL_AFTER_BYTES: u64 = 1 << 20;

pub(super) const RECEIVE_STALL: Duration = Duration::from_secs(60);

/// One deadline for all of a departure's stops, well inside Demote's 120 s timeout.
pub(super) const STOP_DEADLINE: Duration = Duration::from_secs(30);

/// The receive this process started for each Volume, by name, with the target it pulls. An
/// entry lasts while its task or a `zfs` child the task dropped is alive: that child can
/// still write the slot or hold it busy, and only the entry lets the departure wait for it
/// or name it. A plugin process that restarts starts with none.
#[derive(Clone)]
pub(super) struct Receives {
    receives: Arc<Mutex<BTreeMap<String, Entry>>>,
    /// The newest seed run whose Seed of each Volume this process admitted, by gen. It
    /// outlives the entry, since a later run's Seed replaces the entry with its own copy.
    seeds: Arc<Mutex<BTreeMap<String, VolumeGen>>>,
    /// How long a failure waits for the children its task dropped. One still alive after
    /// the departure's own deadline for a stop is stuck, not writing.
    hold: Duration,
}

impl Default for Receives {
    fn default() -> Self {
        Self {
            receives: Arc::default(),
            seeds: Arc::default(),
            hold: STOP_DEADLINE,
        }
    }
}

/// One Volume's receive and the snapshot it pulls. Its ending is that target's alone, so a
/// failure is answered only to a pull of the same target.
struct Entry {
    target: SnapshotName,
    receive: Receive,
    /// The seed runs its failure was answered to.
    answered: Answered,
}

/// Which seed runs a failure was answered to, by gen. A Seed carrying no gen names no
/// run, which reads as `To(None)`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Answered {
    #[default]
    Not,
    /// The newest run it was answered to.
    To(Option<VolumeGen>),
}

impl Answered {
    /// Whether a Seed of `run` comes from a run after every one this failure was answered
    /// to. A failure no run was answered keeps until one is. Without a gen a Seed cannot
    /// tell a resend of the run it answered from a later run, so it counts as later.
    fn superseded_by(self, run: Option<VolumeGen>) -> bool {
        match (self, run) {
            (Self::Not, _) => false,
            (Self::To(Some(answered)), Some(run)) => run > answered,
            (Self::To(_), None) | (Self::To(None), Some(_)) => true,
        }
    }

    /// Answered to `run` too.
    fn and(self, run: Option<VolumeGen>) -> Self {
        match self {
            Self::To(answered) => Self::To(answered.max(run)),
            Self::Not => Self::To(run),
        }
    }
}

/// One Volume's receive. Its task writes the ending before it lets go of its life, so a
/// `Running` receive's life is never finished.
enum Receive {
    Running {
        life: Life,
    },
    /// The task returned. Its life finishes once every child it dropped has exited.
    Ended {
        ending: Ending,
        at: Instant,
        life: Life,
    },
}

enum Ending {
    Landed,
    Failed(String),
    /// A departure stopped the task before it landed.
    Stopped,
}

/// A task's stop signal and its finish, which fires once the task returned and every
/// child it dropped has exited.
struct Life {
    stop: CancellationToken,
    finished: CancellationToken,
}

impl Receive {
    fn life(&self) -> &Life {
        match self {
            Self::Running { life } | Self::Ended { life, .. } => life,
        }
    }

    fn unfinished(&self) -> bool {
        !self.life().finished.is_cancelled()
    }

    fn end(self, ending: Ending) -> Self {
        match self {
            Self::Running { life } => Self::Ended {
                ending,
                at: Instant::now(),
                life,
            },
            ended @ Self::Ended { .. } => ended,
        }
    }
}

/// What a route reads of a Volume's receive, or of its receive of one target.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Status {
    /// No receive, or one that landed or was stopped and left no child alive. Read for one
    /// target, a receive of another target reads as none.
    Idle,
    /// The task, or a child it dropped, is alive, with no failure to show yet.
    Receiving,
    /// `settled` is false while a child the task dropped outlives the hold: the failure
    /// shows, but that child can still hold the slot.
    Failed { failure: String, settled: bool },
}

impl Status {
    /// Whether the task or a child it dropped can still change the slot.
    pub(super) fn busy(&self) -> bool {
        matches!(self, Self::Receiving | Self::Failed { settled: false, .. })
    }

    #[cfg(test)]
    pub(super) fn failure(self) -> Option<String> {
        match self {
            Self::Failed { failure, .. } => Some(failure),
            Self::Idle | Self::Receiving => None,
        }
    }
}

/// The receive's task, or a child it dropped, is still alive.
#[derive(Debug)]
pub(super) struct Busy;

/// A receive task's stop signal and a share of its finish. The task has finished once
/// every share is gone, and a `zfs` child dropped unreaped keeps one until it exits.
#[derive(Clone)]
pub(super) struct Stop {
    token: CancellationToken,
    /// Set once the task acted on the stop, so the error it then returns names the stop,
    /// not a fault of the receive.
    heeded: Arc<AtomicBool>,
    finish: Arc<DropGuard>,
}

impl Stop {
    /// A stop no receive task waits on.
    #[cfg(test)]
    pub(super) fn unowned() -> Self {
        Self {
            token: CancellationToken::new(),
            heeded: Arc::default(),
            finish: Arc::new(CancellationToken::new().drop_guard()),
        }
    }

    /// Fires once [`Receives::stop_all`] stops the task. A task that acts on it returns
    /// an error, which the receive records as stopped rather than failed. A caller returns
    /// as soon as it fires, or a failure of its own after that would be recorded as stopped.
    pub(super) async fn cancelled(&self) {
        self.token.cancelled().await;
        self.heeded.store(true, Ordering::Relaxed);
    }

    fn heeded(&self) -> bool {
        self.heeded.load(Ordering::Relaxed)
    }
}

impl Receives {
    #[cfg(test)]
    pub(super) fn holding(hold: Duration) -> Self {
        Self {
            hold,
            ..Self::default()
        }
    }

    /// Whether a departure has stopped `name`'s receive.
    #[cfg(test)]
    pub(super) fn stopped(&self, name: &VolumeName) -> bool {
        self.receives
            .lock()
            .expect("receive registry is not poisoned")
            .get(name.as_str())
            .is_some_and(|entry| entry.receive.life().stop.is_cancelled())
    }

    /// The receive of `name`, whatever its target. A task that answered still runs until
    /// the children it dropped exit, and those can still change the slot, so its failure
    /// waits for them, but only for the hold.
    pub(super) fn status(&self, name: &VolumeName) -> Status {
        let receives = self
            .receives
            .lock()
            .expect("receive registry is not poisoned");
        receives
            .get(name.as_str())
            .map_or(Status::Idle, |entry| self.read(&entry.receive))
    }

    /// The receive of `name` pulling `target`, read as [`Self::status`] reads it.
    pub(super) fn status_of(&self, name: &VolumeName, target: &SnapshotName) -> Status {
        let receives = self
            .receives
            .lock()
            .expect("receive registry is not poisoned");
        match receives.get(name.as_str()) {
            Some(entry) if entry.target == *target => self.read(&entry.receive),
            Some(_) | None => Status::Idle,
        }
    }

    fn read(&self, receive: &Receive) -> Status {
        let settled = !receive.unfinished();
        match receive {
            Receive::Running { .. } => Status::Receiving,
            Receive::Ended {
                ending: Ending::Failed(failure),
                at,
                ..
            } if settled || at.elapsed() >= self.hold => Status::Failed {
                failure: failure.clone(),
                settled,
            },
            Receive::Ended { .. } if settled => Status::Idle,
            Receive::Ended { .. } => Status::Receiving,
        }
    }

    /// Answers the failure of `name`'s receive of `target` and clears it in one step, so
    /// the answer is given once and the next start of the receive begins afresh. A failure
    /// of another target, or with no target named, is not answered and stays.
    ///
    /// # Errors
    ///
    /// [`Busy`] while the task or a child it dropped is alive, whatever its target and
    /// whether or not its failure already shows.
    pub(super) fn take_failure(
        &self,
        name: &VolumeName,
        target: Option<&SnapshotName>,
    ) -> Result<Option<String>, Busy> {
        let mut receives = self
            .receives
            .lock()
            .expect("receive registry is not poisoned");
        match receives.remove(name.as_str()) {
            None => Ok(None),
            Some(Entry {
                target: failed,
                receive:
                    Receive::Ended {
                        ending: Ending::Failed(failure),
                        life,
                        ..
                    },
                ..
            }) if Some(&failed) == target && life.finished.is_cancelled() => Ok(Some(failure)),
            Some(entry) => {
                let busy = entry.receive.unfinished();
                receives.insert(name.to_string(), entry);
                if busy { Err(Busy) } else { Ok(None) }
            }
        }
    }

    /// Admits a Seed of `name` from the seed run `run`, which becomes the newest unless
    /// one after it was admitted, whose gen is the refusal. Cloud takes a run's gen before
    /// it admits the run, and a Seed can wait in transit past its run, so a Seed of an
    /// earlier run can reach this Machine after a later run's. Refused, it neither answers
    /// the later run's failure to its own run nor starts the later run's copy over. A Seed
    /// without a gen names no run and is admitted.
    pub(super) fn admit_seed(
        &self,
        name: &VolumeName,
        run: Option<VolumeGen>,
    ) -> Result<(), VolumeGen> {
        let Some(run) = run else {
            return Ok(());
        };
        let mut seeds = self.seeds.lock().expect("seed fence is not poisoned");
        let newest = seeds.entry(name.to_string()).or_insert(run);
        if run < *newest {
            return Err(*newest);
        }
        *newest = run;
        Ok(())
    }

    /// The failure of `name`'s receive of `target` a Seed of `run` answers, read as
    /// [`Self::take_failure`] reads it. Unlike that read, it stays: a reply can be lost on
    /// its way to Cloud, which then resends the same run's Seed, and that Seed must answer
    /// the failure again rather than start over. It goes only when a Seed of a later run
    /// than every one it was answered to reads it: that run began after the earlier one
    /// ended, so the copy starts over at Cloud's ask. An answer records `run` for the
    /// failure in the same step, so only a reply showing this answer shows the failure.
    ///
    /// # Errors
    ///
    /// [`Busy`] as [`Self::take_failure`] answers it, which records nothing, though the
    /// failure may already show.
    pub(super) fn seed_failure(
        &self,
        name: &VolumeName,
        target: Option<&SnapshotName>,
        run: Option<VolumeGen>,
    ) -> Result<Option<String>, Busy> {
        let mut receives = self
            .receives
            .lock()
            .expect("receive registry is not poisoned");
        let Some(entry) = receives.get_mut(name.as_str()) else {
            return Ok(None);
        };
        match entry {
            Entry {
                target: failed,
                receive:
                    Receive::Ended {
                        ending: Ending::Failed(failure),
                        life,
                        ..
                    },
                answered,
            } if Some(&*failed) == target && life.finished.is_cancelled() => {
                if answered.superseded_by(run) {
                    receives.remove(name.as_str());
                    Ok(None)
                } else {
                    *answered = answered.and(run);
                    Ok(Some(failure.clone()))
                }
            }
            entry if entry.receive.unfinished() => Err(Busy),
            _ => Ok(None),
        }
    }

    /// Drops `name`'s receive once it has finished. One whose task or dropped child is
    /// alive stays, so the departure still waits for it and its ending is not lost.
    pub(super) fn forget(&self, name: &VolumeName) {
        let mut receives = self
            .receives
            .lock()
            .expect("receive registry is not poisoned");
        if receives
            .get(name.as_str())
            .is_some_and(|entry| !entry.receive.unfinished())
        {
            receives.remove(name.as_str());
        }
    }

    /// Runs `work` as the receive task of `name` pulling `target`, which its ending belongs
    /// to. A receive starts only under the plugin's mutation, which the departure holds
    /// while it stops them all. `work` gets the [`Stop`] [`Self::stop_all`] fires; once it
    /// does, `work` kills and reaps its `zfs` children before returning, and an error it
    /// returns after acting on the stop ends the receive stopped. A panic is recorded as
    /// the task's failure.
    ///
    /// # Errors
    ///
    /// [`Busy`] while the receive of `name`, or a child it dropped, is alive. `work` does
    /// not run then.
    pub(super) fn spawn<F>(
        &self,
        _held: &HeldMutation,
        name: &VolumeName,
        target: &SnapshotName,
        work: impl FnOnce(Stop) -> F,
    ) -> Result<(), Busy>
    where
        F: Future<Output = Result<(), String>> + Send + 'static,
    {
        let life = Life {
            stop: CancellationToken::new(),
            finished: CancellationToken::new(),
        };
        let stop = Stop {
            token: life.stop.clone(),
            heeded: Arc::default(),
            finish: Arc::new(life.finished.clone().drop_guard()),
        };
        {
            let mut receives = self
                .receives
                .lock()
                .expect("receive registry is not poisoned");
            if receives
                .get(name.as_str())
                .is_some_and(|entry| entry.receive.unfinished())
            {
                return Err(Busy);
            }
            receives.insert(
                name.to_string(),
                Entry {
                    target: target.clone(),
                    receive: Receive::Running { life },
                    answered: Answered::Not,
                },
            );
        }
        let held = stop.clone();
        let work = work(stop);
        let receives = Arc::clone(&self.receives);
        let name = name.to_string();
        tokio::spawn(async move {
            let ending = match std::panic::AssertUnwindSafe(work).catch_unwind().await {
                Ok(Ok(())) => Ending::Landed,
                Ok(Err(_)) if held.heeded() => Ending::Stopped,
                Ok(Err(failure)) => Ending::Failed(failure),
                Err(_) => Ending::Failed("the receive task panicked".to_owned()),
            };
            let mut receives = receives.lock().expect("receive registry is not poisoned");
            if let Some(mut entry) = receives.remove(&name) {
                entry.receive = entry.receive.end(ending);
                receives.insert(name, entry);
            }
            drop(receives);
            // The task's share of the finish goes only once the ending is written, so a
            // finished receive is never `Running`.
            drop(held);
        });
        Ok(())
    }

    /// Stops every receive task and waits until each ended and its `zfs` children
    /// exited, so ZFS holds no slot busy and no receive changes a slot afterwards. Answers
    /// the names with a task still running once `deadline` passes.
    pub(super) async fn stop_all(&self, deadline: Duration) -> Vec<String> {
        let finished: Vec<(String, CancellationToken)> = self
            .receives
            .lock()
            .expect("receive registry is not poisoned")
            .iter()
            .map(|(name, entry)| {
                let life = entry.receive.life();
                life.stop.cancel();
                (name.clone(), life.finished.clone())
            })
            .collect();
        let deadline = Instant::now() + deadline;
        let mut stuck = Vec::new();
        for (name, finished) in finished {
            if tokio::time::timeout_at(deadline, finished.cancelled())
                .await
                .is_err()
            {
                stuck.push(name);
            }
        }
        stuck
    }
}

/// An origin as a message names it; `None` is Legacy.
pub(super) fn shown_origin(origin: Option<ployz_core::VolumeOrigin>) -> String {
    origin.map_or_else(|| "Legacy".to_owned(), |origin| origin.to_string())
}

/// A stream the writer answered 200 for, not yet read.
pub(super) struct OpenedStream {
    /// The stream's URL as [`SendStream::shown`] prints it.
    shown: String,
    response: reqwest::Response,
}

/// A writer's answer to [`VolumeStorage::sends_as`].
pub(super) enum Proof {
    /// It sends the target as the snapshot this Machine holds.
    Sent,
    /// It has no such snapshot, or sends another under its name.
    Refuted(String),
}

/// Why a receive did not land its stream.
#[derive(Debug, thiserror::Error)]
pub(super) enum ReceiveFailure {
    /// The writer has no such stream: the base or the token is gone from it.
    #[error("{0}")]
    NoStream(String),
    #[error("{0}")]
    Failed(String),
    /// This Server's own ZFS read or write of the slot failed.
    #[error(transparent)]
    Storage(#[from] PluginError),
}

impl VolumeStorage {
    /// Pulls one stream into `fs`, then re-asserts and verifies the slot's bound and
    /// read-only state, which ZFS drops on an interrupted first receive. The
    /// `SetVolumeCopy.MirrorToMirror` kill point fires once the stream passes
    /// [`KILL_AFTER_BYTES`], for the Volume and target `stream` names in lane `r#gen`.
    pub(super) async fn receive(
        &self,
        from: ManagementAddress,
        stream: &SendStream,
        (parent, fs): (&str, &str),
        bound: u64,
        r#gen: VolumeGen,
        stop: &Stop,
    ) -> Result<(), ReceiveFailure> {
        let opened = self.open_stream(from, stream, stop).await?;
        self.admit_origin(parent, fs, &opened).await?;
        self.receive_opened(opened, fs, bound, pulling(stream, r#gen), stop)
            .await
    }

    /// Admits the origin `opened` carries into the slot `parent`, before any byte of it is
    /// piped: a slot with a copy or a record must already carry that origin. A refusal
    /// changes nothing. Otherwise the record becomes exactly that origin, so a crash from
    /// here on reads the bytes as what the writer said they are.
    pub(super) async fn admit_origin(
        &self,
        parent: &str,
        fs: &str,
        opened: &OpenedStream,
    ) -> Result<(), ReceiveFailure> {
        let failed = |error: String| ReceiveFailure::Failed(error);
        let sent = ployz_core::single_origin(
            opened
                .response
                .headers()
                .get_all(ployz_core::ORIGIN_HEADER)
                .iter()
                .map(|value| value.to_str().unwrap_or_default()),
        )
        .map_err(|error| failed(format!("{} answered {error}", opened.shown)))?;
        let record = self.record(parent).await?;
        // An absent `fs` lets any origin in, so absence comes only from a finished listing.
        let held = match record {
            Some(record) => Some(Some(record)),
            None if self.exists(fs).await? => {
                let newest = self.newest_snapshot(fs).await?;
                Some(
                    self.origin_of(None, Some((fs, newest.as_ref().map(|newest| &newest.name))))
                        .await?,
                )
            }
            None => None,
        };
        if let Some(held) = held
            && held != sent
        {
            return Err(failed(format!(
                "{} sent data of origin {}, and {fs} holds origin {}; this Server keeps its copy",
                opened.shown,
                shown_origin(sent),
                shown_origin(held)
            )));
        }
        if record == sent {
            return Ok(());
        }
        Ok(self.write_record(parent, sent).await?)
    }

    /// The writer's answer to `stream`, before anything touches the slot. A writer that
    /// never answers holds the departure only until `stop` fires.
    pub(super) async fn open_stream(
        &self,
        from: ManagementAddress,
        stream: &SendStream,
        stop: &Stop,
    ) -> Result<OpenedStream, ReceiveFailure> {
        tokio::select! {
            biased;
            () = stop.cancelled() => Err(ReceiveFailure::Failed(format!(
                "the request for {} was stopped",
                stream.shown()
            ))),
            opened = self.request_stream(from, stream) => opened,
        }
    }

    /// Whether `from` sends `stream`'s target as the snapshot `guid`. A send opens with a
    /// BEGIN record naming the snapshot it builds, and a receive keeps the sender's guid,
    /// so a match proves `from` holds the point this Machine holds as `guid`. Only that
    /// record is read: dropping the response stops the send. An error is no answer either
    /// way, such as a writer that can't be reached.
    pub(super) async fn sends_as(
        &self,
        from: ManagementAddress,
        stream: &SendStream,
        guid: SnapshotGuid,
    ) -> Result<Proof, ReceiveFailure> {
        let OpenedStream { shown, response } = match self.request_stream(from, stream).await {
            Ok(opened) => opened,
            Err(ReceiveFailure::NoStream(failure)) => return Ok(Proof::Refuted(failure)),
            Err(failure) => return Err(failure),
        };
        let mut body = tokio_util::io::StreamReader::new(
            response
                .bytes_stream()
                .map(|chunk| chunk.map_err(std::io::Error::other)),
        );
        match crate::volume_send::begin::read(&mut body, guid.get(), self.receive_stall).await {
            Ok(_) => Ok(Proof::Sent),
            Err(error @ crate::volume_send::begin::BeginError::OtherSnapshot { .. }) => {
                Ok(Proof::Refuted(format!("{shown}: {error}")))
            }
            Err(error) => Err(ReceiveFailure::Failed(format!("{shown}: {error}"))),
        }
    }

    async fn request_stream(
        &self,
        from: ManagementAddress,
        stream: &SendStream,
    ) -> Result<OpenedStream, ReceiveFailure> {
        let address = format!("http://[{}]:{}", from.0, self.send_port);
        let url = format!("{address}{}", stream.path_and_query());
        // Messages name the stream as `shown`, and reqwest's errors lose their URL, so a
        // resume token never leaves this Machine's request.
        let shown = format!("{address}{}", stream.shown());
        let client = reqwest::Client::builder()
            .connect_timeout(self.receive_stall)
            .read_timeout(self.receive_stall)
            .build()
            .map_err(|error| {
                ReceiveFailure::Failed(format!(
                    "could not build the receive client: {}",
                    inline(&error)
                ))
            })?;
        // This Server checks origins at create and at every rename, so it takes born data.
        let response = client
            .get(&url)
            .header(
                ployz_core::CAPABILITIES_HEADER,
                ployz_core::VOLUME_ORIGIN_CAPABILITY,
            )
            .send()
            .await
            .map_err(|error| {
                ReceiveFailure::Failed(format!("GET {shown}: {}", inline(&error.without_url())))
            })?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            let reason = response.text().await.unwrap_or_default();
            return Err(ReceiveFailure::NoStream(format!("GET {shown}: {reason}")));
        }
        let response = response.error_for_status().map_err(|error| {
            ReceiveFailure::Failed(format!("GET {shown}: {}", inline(&error.without_url())))
        })?;
        Ok(OpenedStream { shown, response })
    }

    /// Pulls an opened stream into `fs`. Once `stop` fires, the receive is killed and
    /// reaped before this returns.
    pub(super) async fn receive_opened(
        &self,
        OpenedStream { shown, response }: OpenedStream,
        fs: &str,
        bound: u64,
        identity: Identity<'_>,
        stop: &Stop,
    ) -> Result<(), ReceiveFailure> {
        let mut child = self
            .spawn_receive(fs, bound, Stdio::piped(), stop)
            .map_err(|error| {
                ReceiveFailure::Failed(format!("could not run zfs receive: {error}"))
            })?;
        let pumped = {
            let pump = self.pump(child.child(), &shown, response, fs, identity);
            tokio::select! {
                biased;
                () = stop.cancelled() => None,
                pumped = pump => Some(pumped),
            }
        };
        child.reap().await;
        pumped.ok_or_else(|| {
            ReceiveFailure::Failed(format!("the receive into {fs} was stopped"))
        })??;
        Ok(self.finish_copy(fs, bound).await?)
    }

    /// Writes the stream into the receive `child` and waits for it to exit.
    async fn pump(
        &self,
        child: &mut Child,
        shown: &str,
        response: reqwest::Response,
        fs: &str,
        identity: Identity<'_>,
    ) -> Result<(), ReceiveFailure> {
        let mut stdin = child.stdin.take().expect("zfs receive stdin is piped");
        let mut body = response.bytes_stream();
        let mut received = 0u64;
        let mut broke = None;
        while let Some(chunk) = body.next().await {
            match chunk {
                Ok(bytes) => {
                    match tokio::time::timeout(self.receive_stall, stdin.write_all(&bytes)).await {
                        Ok(Ok(())) => {}
                        Ok(Err(error)) => {
                            broke = Some(error.to_string());
                            break;
                        }
                        Err(_) => {
                            return Err(ReceiveFailure::Failed(format!(
                                "zfs receive into {fs} stalled writing after {received} bytes"
                            )));
                        }
                    }
                    let before = received;
                    received += bytes.len() as u64;
                    if before < KILL_AFTER_BYTES && received >= KILL_AFTER_BYTES {
                        crate::faults::kill_inside(
                            Point::Copy(CopyTransition::MirrorToMirror),
                            Some(identity),
                        );
                    }
                }
                Err(error) => {
                    broke = Some(inline(&error.without_url()));
                    break;
                }
            }
        }
        drop(stdin);
        let (status, stderr) = tokio::time::timeout(self.receive_stall, exited(child))
            .await
            .map_err(|_| {
                ReceiveFailure::Failed(format!(
                    "zfs receive into {fs} stalled finishing after {received} bytes"
                ))
            })?
            .map_err(|error| {
                ReceiveFailure::Failed(format!("zfs receive did not finish: {error}"))
            })?;
        if !status.success() {
            return Err(ReceiveFailure::Failed(format!(
                "zfs receive into {fs} failed after {received} bytes: {}{}",
                without_resume_token(stderr.trim()),
                broke.map_or(String::new(), |error| format!(" (stream: {error})"))
            )));
        }
        if let Some(error) = broke {
            return Err(ReceiveFailure::Failed(format!(
                "stream from {shown} broke after {received} bytes: {error}"
            )));
        }
        Ok(())
    }

    /// The copy is read-only and carries the slot's bound, whatever the stream said.
    pub(super) async fn finish_copy(&self, fs: &str, bound: u64) -> super::Result<()> {
        for (property, value) in [("readonly", "on"), ("refquota", bound.to_string().as_str())] {
            self.set_verified(fs, property, value).await?;
        }
        Ok(())
    }
}

/// A `zfs` process that has exited by the time its owner is gone, on every path out:
/// one still exiting holds its dataset busy for the next destroy.
pub(super) struct ZfsChild {
    child: Option<Child>,
    finish: Arc<DropGuard>,
    #[cfg(test)]
    spared: bool,
    #[cfg(test)]
    kill_fails: bool,
    #[cfg(test)]
    wait_fails: bool,
}

impl ZfsChild {
    /// Spawns a child of the receive task `stop` belongs to, which finishes only once the
    /// child has exited.
    pub(super) fn spawn(command: &mut Command, stop: &Stop) -> std::io::Result<Self> {
        command.spawn().map(|child| Self {
            child: Some(child),
            finish: Arc::clone(&stop.finish),
            #[cfg(test)]
            spared: false,
            #[cfg(test)]
            kill_fails: false,
            #[cfg(test)]
            wait_fails: false,
        })
    }

    pub(super) fn child(&mut self) -> &mut Child {
        self.child
            .as_mut()
            .expect("a zfs child is held until reaped")
    }

    #[cfg(test)]
    pub(super) fn id(&self) -> Option<u32> {
        self.child.as_ref().and_then(Child::id)
    }

    /// Leaves the child to exit on its own clock when dropped. No process a test can
    /// start outlives SIGKILL; a `zfs` blocked in the kernel does, and this stands in.
    #[cfg(test)]
    pub(super) fn spare_from_kill(&mut self) {
        self.spared = true;
    }

    /// Makes the kill on a reap or a drop fail, as one refused by the kernel does.
    #[cfg(test)]
    pub(super) fn fail_kill(&mut self) {
        self.kill_fails = true;
    }

    /// Makes the first wait for the child's exit fail, as a `waitpid` error does.
    #[cfg(test)]
    pub(super) fn fail_wait(&mut self) {
        self.wait_fails = true;
    }

    /// Kills the process unless it already exited, and waits for it. A failed kill still
    /// waits, holding the task's finish, so the departure names a child it cannot stop.
    pub(super) async fn reap(mut self) {
        let Some(mut exit) = self.exit() else {
            return;
        };
        if let Err(error) = self.start_kill(&mut exit.child) {
            tracing::warn!(%error, "could not kill a zfs process");
        }
        exit.held().await;
    }

    fn exit(&mut self) -> Option<Exit> {
        Some(Exit {
            child: self.child.take()?,
            finish: Arc::clone(&self.finish),
            #[cfg(test)]
            wait_fails: self.wait_fails,
        })
    }

    fn start_kill(&self, child: &mut Child) -> std::io::Result<()> {
        #[cfg(test)]
        if self.kill_fails {
            return Err(std::io::Error::other("injected kill failure"));
        }
        match child.start_kill() {
            // Already waited for.
            Err(error) if error.kind() == std::io::ErrorKind::InvalidInput => Ok(()),
            result => result,
        }
    }
}

/// Only an early return or a panic gets here; every other path reaps. Dropping cannot
/// await, so a reaper waits for the exit and holds the task's finish until then.
impl Drop for ZfsChild {
    fn drop(&mut self) {
        let Some(mut exit) = self.exit() else {
            return;
        };
        #[cfg(test)]
        let spared = self.spared;
        #[cfg(not(test))]
        let spared = false;
        if !spared && let Err(error) = self.start_kill(&mut exit.child) {
            tracing::warn!(%error, "could not kill a zfs process");
        }
        tokio::spawn(exit.held());
    }
}

/// A child on its way out, holding its task's finish.
struct Exit {
    child: Child,
    finish: Arc<DropGuard>,
    #[cfg(test)]
    wait_fails: bool,
}

impl Exit {
    /// Releases the finish only once a wait proves the child exited. A failed wait proves
    /// nothing, so it is retried, and until one succeeds the departure names the child.
    async fn held(mut self) {
        let mut warned = false;
        loop {
            #[cfg(test)]
            let waited = if std::mem::take(&mut self.wait_fails) {
                Err(std::io::Error::other("injected wait failure"))
            } else {
                self.child.wait().await
            };
            #[cfg(not(test))]
            let waited = self.child.wait().await;
            match waited {
                Ok(_) => break,
                Err(error) => {
                    if !std::mem::replace(&mut warned, true) {
                        tracing::warn!(%error, "could not wait for a zfs process; retrying");
                    }
                    tokio::time::sleep(WAIT_RETRY).await;
                }
            }
        }
        drop(self.finish);
    }
}

const WAIT_RETRY: Duration = Duration::from_secs(1);

/// The exit status and stderr of `child`, which keeps the child to kill if the wait is
/// abandoned.
pub(super) async fn exited(
    child: &mut Child,
) -> std::io::Result<(std::process::ExitStatus, String)> {
    let mut stderr = Vec::new();
    let mut pipe = child.stderr.take();
    let read = async {
        match pipe.as_mut() {
            Some(pipe) => pipe.read_to_end(&mut stderr).await.map(drop),
            None => Ok(()),
        }
    };
    let (status, read) = tokio::join!(child.wait(), read);
    read?;
    Ok((status?, String::from_utf8_lossy(&stderr).into_owned()))
}

/// `zfs receive`'s stderr fit to print or store: OpenZFS names an interrupted
/// receive's resume token in the `zfs send -t <token>` that would resume it.
pub(super) fn without_resume_token(stderr: &str) -> String {
    stderr
        .lines()
        .map(|line| {
            line.split_once("zfs send -t ").map_or_else(
                || line.to_owned(),
                |(before, _)| format!("{before}zfs send -t …"),
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}
