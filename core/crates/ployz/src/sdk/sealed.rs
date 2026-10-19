//! Container log readers that never show a Sealed value. The caller feeds the Sealed names the
//! Store holds into a [`SealedNames`] handle; the readers read each Container's env themselves
//! and hide what they cannot check.
use futures_util::FutureExt;
use futures_util::future::{BoxFuture, Shared};
use ployz_core::{
    ContainerId, HistoryRow, LogBody, LogEntry, LogLevel, LogMetadata, MachineId, RpcError,
    log_level,
};
use ployz_store::tools::{Channel, FrameScrub, SealedForms, SealedValues};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use tokio::sync::{Mutex, Notify, watch};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use ts_rs::TS;

use super::Session;
use super::logs::{
    ContainerLogInput, ContainerLogStream, LogChannel, LogHistoryInput, LogHistoryRecord,
    LogHistoryStream, ROW_DEADLINE, container_env, history_record,
};
use crate::connect::Client;

/// How long a reader waits for names read after its Container's env before it hides.
const NAMES_WAIT: std::time::Duration = ROW_DEADLINE;

/// What the Store held Sealed, as of one read.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Names {
    Unread,
    /// A read failed: nothing is shown until a later read lands.
    Failed,
    /// The Store owns no such Namespace: every env value is scrubbed.
    Every,
    Only(BTreeSet<String>),
}

#[derive(Debug)]
struct Read {
    names: Names,
    /// The `begin` that produced `names`. A read only lands over an older one.
    generation: u64,
}

/// The Sealed names one log request scrubs under, shared by its readers. Knows none until its
/// first read lands; until then readers wait, and once a read fails they hide.
#[derive(Clone)]
pub struct SealedNames {
    shared: Arc<State>,
}

struct State {
    read: watch::Sender<Arc<Read>>,
    begun: AtomicU64,
    wanted: Notify,
    closed: AtomicBool,
    envs: std::sync::Mutex<BTreeMap<(MachineId, ContainerId), EnvRead>>,
    none: Arc<SealedForms>,
}

/// One read of the names in flight. Consumed by `done` or `failed`, so it lands once.
pub struct PendingNames {
    generation: u64,
    shared: Arc<State>,
}

/// One Container's env as its Machine reported it. It never leaves this crate.
struct Env {
    /// None when the Container is gone or its Machine did not answer.
    values: Option<BTreeMap<String, String>>,
    /// The newest `begin` before this env was asked for; only a later read covers it.
    after: u64,
    /// The forms compiled for the newest names, kept while a refresh reads the same names.
    forms: std::sync::Mutex<Option<(Names, Arc<SealedForms>)>>,
}

type EnvRead = Shared<BoxFuture<'static, Arc<Env>>>;

/// Whether a Container's lines can be shown now, and under which forms.
enum Verdict {
    Show(Arc<SealedForms>),
    Wait,
    Hide,
}

impl Default for SealedNames {
    fn default() -> Self {
        Self::new()
    }
}

impl SealedNames {
    #[must_use]
    pub fn new() -> Self {
        Self {
            shared: Arc::new(State {
                read: watch::Sender::new(Arc::new(Read {
                    names: Names::Unread,
                    generation: 0,
                })),
                begun: AtomicU64::new(0),
                wanted: Notify::new(),
                closed: AtomicBool::new(false),
                envs: std::sync::Mutex::default(),
                none: Arc::new(SealedForms::none()),
            }),
        }
    }

    /// Start a read of the names. Ask the Store only after this returns.
    #[must_use]
    pub fn begin(&self) -> PendingNames {
        PendingNames {
            generation: self.shared.begun.fetch_add(1, Ordering::SeqCst) + 1,
            shared: Arc::clone(&self.shared),
        }
    }

    /// Resolves once a reader needs names newer than the last read, or at once after `close`.
    pub async fn wanted(&self) {
        let wanted = self.shared.wanted.notified();
        if !self.shared.closed.load(Ordering::SeqCst) {
            wanted.await;
        }
    }

    /// End the request: every `wanted`, pending or later, resolves at once.
    pub fn close(&self) {
        self.shared.closed.store(true, Ordering::SeqCst);
        self.shared.wanted.notify_waiters();
    }

    fn want(&self) {
        self.shared.wanted.notify_one();
    }

    fn read(&self) -> Arc<Read> {
        Arc::clone(&self.shared.read.borrow())
    }

    /// `container`'s env, read once per request however many readers ask.
    fn env(&self, client: &Client, machine: MachineId, container: ContainerId) -> EnvRead {
        let mut envs = self.shared.envs.lock().expect("sealed env lock");
        envs.entry((machine, container))
            .or_insert_with(|| {
                let after = self.shared.begun.load(Ordering::SeqCst);
                let client = client.clone();
                let values =
                    tokio::spawn(async move { container_env(&client, machine, container).await });
                async move {
                    Arc::new(Env {
                        values: values.await.ok().flatten(),
                        after,
                        forms: std::sync::Mutex::default(),
                    })
                }
                .boxed()
                .shared()
            })
            .clone()
    }

    /// What `env`'s lines may show, waiting until `deadline` for names that cover it.
    async fn settle(
        &self,
        env: &EnvRead,
        opened: Option<u64>,
        deadline: Instant,
        cancel: &CancellationToken,
    ) -> Verdict {
        let mut changes = self.shared.read.subscribe();
        if matches!(changes.borrow().names, Names::Unread) {
            self.want();
        }
        let env = tokio::select! {
            () = cancel.cancelled() => return Verdict::Hide,
            env = tokio::time::timeout_at(deadline, env.clone()) => env.ok(),
        };
        loop {
            let read = Arc::clone(&changes.borrow_and_update());
            let verdict = verdict(&read, env.as_deref(), opened, &self.shared.none);
            if !matches!(verdict, Verdict::Wait) {
                return verdict;
            }
            self.want();
            tokio::select! {
                () = cancel.cancelled() => return Verdict::Hide,
                changed = tokio::time::timeout_at(deadline, changes.changed()) => {
                    if !matches!(changed, Ok(Ok(()))) {
                        return Verdict::Hide;
                    }
                }
            }
        }
    }
}

impl PendingNames {
    /// The Store's answer: the names it holds Sealed, or None when it owns no such Namespace.
    pub fn done(self, names: Option<BTreeSet<String>>) {
        self.land(names.map_or(Names::Every, Names::Only));
    }

    /// The Store could not be read: hide every line until a later read lands.
    pub fn failed(self) {
        self.land(Names::Failed);
    }

    fn land(self, names: Names) {
        let generation = self.generation;
        self.shared.read.send_if_modified(|read| {
            if read.generation > generation {
                return false;
            }
            *read = Arc::new(Read { names, generation });
            true
        });
    }
}

/// The fail-closed table. `env` is None until the Container's env has been read; `opened` is
/// the newest `begin` before a history page opened.
fn verdict(
    read: &Read,
    env: Option<&Env>,
    opened: Option<u64>,
    none: &Arc<SealedForms>,
) -> Verdict {
    match &read.names {
        Names::Failed => Verdict::Hide,
        Names::Unread => Verdict::Wait,
        // Nothing was Sealed when these stored lines were read, so no env could hold a value.
        Names::Only(names)
            if names.is_empty() && opened.is_some_and(|opened| read.generation > opened) =>
        {
            Verdict::Show(Arc::clone(none))
        }
        Names::Every | Names::Only(_) => match env {
            None => Verdict::Wait,
            // A Deploy may have Sealed a name and started this Container after these names were read.
            Some(env) if read.generation <= env.after => Verdict::Wait,
            Some(Env { values: None, .. }) => Verdict::Hide,
            Some(env) => Verdict::Show(env.forms(read)),
        },
    }
}

impl Env {
    fn forms(&self, read: &Read) -> Arc<SealedForms> {
        let mut cached = self.forms.lock().expect("sealed forms lock");
        if let Some((names, forms)) = &*cached
            && *names == read.names
        {
            return Arc::clone(forms);
        }
        let values: SealedValues = self
            .values
            .iter()
            .flatten()
            .filter(|(name, _)| match &read.names {
                Names::Every => true,
                Names::Only(names) => names.contains(*name),
                Names::Unread | Names::Failed => false,
            })
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect();
        let forms = Arc::new(SealedForms::compile(&values));
        *cached = Some((read.names.clone(), Arc::clone(&forms)));
        forms
    }
}

/// A line as it may be shown. `level` is read from the scrubbed text.
#[derive(Clone, Debug, Serialize, TS)]
#[serde(tag = "text", rename_all = "snake_case")]
pub enum LogLine {
    Shown {
        level: LogLevel,
        message: String,
    },
    /// The first line of a run that could not be checked for Sealed values. The rest of the
    /// run is dropped; the next shown line ends it.
    Hidden,
}

/// A Container's output as the viewer may show it.
#[derive(Clone, Debug, Serialize, TS)]
pub struct LiveLogRecord {
    pub source: LogMetadata,
    pub timestamp_nanos: String,
    pub channel: LogChannel,
    #[serde(flatten)]
    pub line: LogLine,
}

/// A live Container log reader that scrubs Sealed values and hides what it cannot check.
pub struct ScrubbedLogStream {
    raw: ContainerLogStream,
    names: SealedNames,
    env: Arc<Env>,
    read: Mutex<LiveRead>,
}

#[derive(Default)]
struct LiveRead {
    scrub: FrameScrub,
    hiding: bool,
    /// The newest frame's source and time, which bytes released at the end carry.
    last: Option<(LogMetadata, i64)>,
    ready: VecDeque<Result<LiveLogRecord, RpcError>>,
    ended: bool,
}

/// A history page reader that scrubs Sealed values and hides the lines it cannot check.
pub struct ScrubbedHistoryStream {
    raw: LogHistoryStream,
    names: SealedNames,
    client: Client,
    machine: MachineId,
    opened: u64,
    read: Mutex<HistoryRead>,
}

#[derive(Default)]
struct HistoryRead {
    envs: BTreeMap<ContainerId, EnvRead>,
    deadline: Option<Instant>,
}

impl Session {
    /// [`Session::container_logs`], scrubbed of the Sealed values in `names`. Reads the
    /// Container's env and waits for names that cover it before the Docker stream opens.
    /// # Errors
    /// Returns invalid input, closed-session, or Machine transport errors.
    pub async fn scrubbed_logs(
        &self,
        input: ContainerLogInput,
        names: &SealedNames,
    ) -> Result<ScrubbedLogStream, RpcError> {
        let client = self.client()?;
        let env = names.env(&client, input.machine_id, input.container_id);
        let deadline = Instant::now() + NAMES_WAIT;
        names.settle(&env, None, deadline, &self.inner.cancel).await;
        let env = env.await;
        Ok(ScrubbedLogStream {
            raw: self.container_logs(input).await?,
            names: names.clone(),
            env,
            read: Mutex::default(),
        })
    }

    /// [`Session::log_history`], scrubbed of the Sealed values in `names`.
    /// # Errors
    /// Returns invalid input, closed-session, or Machine transport errors.
    pub async fn scrubbed_history(
        &self,
        input: LogHistoryInput,
        names: &SealedNames,
    ) -> Result<ScrubbedHistoryStream, RpcError> {
        let client = self.client()?;
        let machine = input.machine_id;
        let opened = names.shared.begun.load(Ordering::SeqCst);
        Ok(ScrubbedHistoryStream {
            raw: self.log_history(input).await?,
            names: names.clone(),
            client,
            machine,
            opened,
            read: Mutex::default(),
        })
    }
}

impl ScrubbedLogStream {
    /// The next output record, skipping heartbeats.
    /// # Errors
    /// Returns malformed-frame and Machine transport failures, after what was held is shown.
    pub async fn next(&self) -> Result<Option<LiveLogRecord>, RpcError> {
        let mut read = self.read.lock().await;
        loop {
            if let Some(record) = read.ready.pop_front() {
                return record.map(Some);
            }
            if read.ended {
                return Ok(None);
            }
            match self.raw.entry().await {
                Ok(Some(entry)) => read.take(entry, self.verdict()),
                Ok(None) => read.end(self.verdict()),
                Err(error) => {
                    read.end(self.verdict());
                    read.ready.push_back(Err(error));
                }
            }
        }
    }

    /// Stop this reader only.
    pub fn cancel(&self) {
        self.raw.cancel();
    }

    fn verdict(&self) -> Verdict {
        match verdict(
            &self.names.read(),
            Some(&self.env),
            None,
            &self.names.shared.none,
        ) {
            Verdict::Wait => Verdict::Hide,
            verdict @ (Verdict::Show(_) | Verdict::Hide) => verdict,
        }
    }
}

impl LiveRead {
    fn take(&mut self, entry: LogEntry, verdict: Verdict) {
        let (channel, bytes) = match entry.body {
            LogBody::Stdout(bytes) => (Channel::Stdout, bytes),
            LogBody::Stderr(bytes) => (Channel::Stderr, bytes),
            LogBody::Error(message) => {
                self.end(verdict);
                self.ready.push_back(Ok(LiveLogRecord {
                    source: entry.metadata,
                    timestamp_nanos: entry.timestamp_unix_nanos.to_string(),
                    channel: LogChannel::Error,
                    line: LogLine::Shown {
                        level: LogLevel::Error,
                        message,
                    },
                }));
                return;
            }
            LogBody::Heartbeat => return,
        };
        self.last = Some((entry.metadata.clone(), entry.timestamp_unix_nanos));
        match verdict {
            Verdict::Show(forms) => {
                let shown = self.scrub.push(channel, &bytes, &forms);
                self.show(channel, shown, entry.metadata, entry.timestamp_unix_nanos);
            }
            Verdict::Wait | Verdict::Hide => {
                self.scrub = FrameScrub::new();
                self.hide(channel, entry.metadata, entry.timestamp_unix_nanos);
            }
        }
    }

    /// Show what is held, scrubbed, and end the stream.
    fn end(&mut self, verdict: Verdict) {
        self.ended = true;
        let scrub = std::mem::take(&mut self.scrub);
        let Some((source, timestamp)) = self.last.take() else {
            return;
        };
        match verdict {
            Verdict::Show(forms) => {
                for (channel, shown) in scrub.finish(&forms) {
                    self.show(channel, shown, source.clone(), timestamp);
                }
            }
            Verdict::Wait | Verdict::Hide => {
                for (channel, _) in scrub.finish(&SealedForms::none()) {
                    self.hide(channel, source.clone(), timestamp);
                }
            }
        }
    }

    fn show(&mut self, channel: Channel, shown: Vec<u8>, source: LogMetadata, timestamp: i64) {
        if shown.is_empty() {
            return;
        }
        self.hiding = false;
        self.ready.push_back(Ok(LiveLogRecord {
            source,
            timestamp_nanos: timestamp.to_string(),
            channel: log_channel(channel),
            line: LogLine::Shown {
                level: log_level(&shown),
                message: String::from_utf8_lossy(&shown).into_owned(),
            },
        }));
    }

    fn hide(&mut self, channel: Channel, source: LogMetadata, timestamp: i64) {
        if std::mem::replace(&mut self.hiding, true) {
            return;
        }
        self.ready.push_back(Ok(LiveLogRecord {
            source,
            timestamp_nanos: timestamp.to_string(),
            channel: log_channel(channel),
            line: LogLine::Hidden,
        }));
    }
}

const fn log_channel(channel: Channel) -> LogChannel {
    match channel {
        Channel::Stdout => LogChannel::Stdout,
        Channel::Stderr => LogChannel::Stderr,
    }
}

impl Drop for ScrubbedLogStream {
    fn drop(&mut self) {
        self.raw.cancel();
    }
}

impl ScrubbedHistoryStream {
    /// The next row, skipping heartbeats. `None` after `end`. A line that cannot be checked for
    /// Sealed values comes as `hidden`, without its text.
    /// # Errors
    /// Returns the store's error row, malformed frames, transport failures,
    /// and a timeout when the store sends no frame for ten seconds.
    pub async fn next(&self) -> Result<Option<LogHistoryRecord>, RpcError> {
        let Some(row) = self.raw.row().await? else {
            return Ok(None);
        };
        let mut read = self.read.lock().await;
        let (container_id, ts, stream, text) = match row {
            HistoryRow::Container(container) => {
                self.env(&mut read, container.container_id);
                return Ok(Some(LogHistoryRecord::Container(container)));
            }
            HistoryRow::Line {
                container_id,
                ts,
                stream,
                text,
            } => (container_id, ts, stream, text),
            row @ (HistoryRow::Gap { .. }
            | HistoryRow::Exit { .. }
            | HistoryRow::End { .. }
            | HistoryRow::Heartbeat
            | HistoryRow::Error(_)) => return Ok(Some(history_record(row))),
        };
        let env = self.env(&mut read, container_id).clone();
        let deadline = *read
            .deadline
            .get_or_insert_with(|| Instant::now() + NAMES_WAIT);
        let verdict = self
            .names
            .settle(&env, Some(self.opened), deadline, self.raw.cancelled())
            .await;
        let timestamp_nanos = ts.to_string();
        Ok(Some(match verdict {
            Verdict::Show(forms) => {
                let message = forms.scrub(&String::from_utf8_lossy(&text), &mut 0);
                LogHistoryRecord::Line {
                    container_id,
                    timestamp_nanos,
                    stream,
                    level: log_level(message.as_bytes()),
                    message,
                }
            }
            Verdict::Wait | Verdict::Hide => LogHistoryRecord::Hidden {
                container_id,
                timestamp_nanos,
            },
        }))
    }

    /// Stop this read only.
    pub fn cancel(&self) {
        self.raw.cancel();
    }

    /// `container`'s env read, started on its first row.
    fn env<'read>(&self, read: &'read mut HistoryRead, container: ContainerId) -> &'read EnvRead {
        read.envs
            .entry(container)
            .or_insert_with(|| self.names.env(&self.client, self.machine, container))
    }
}

#[cfg(test)]
#[path = "sealed_tests.rs"]
mod tests;
