//! Container log readers that never show a Sealed value. The caller feeds the Sealed names the
//! Store holds into a [`SealedNames`] handle; the readers read each Container's env themselves
//! and hide what they cannot check.
use futures_util::FutureExt;
use futures_util::future::{BoxFuture, Shared};
use ployz_core::{
    ContainerId, LogBody, LogEntry, LogLevel, LogMetadata, MachineId, RpcError, log_level,
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
use super::logs::{ContainerLogInput, ContainerLogStream, LogChannel, container_env};
use crate::connect::Client;

/// How long a reader waits for names read after its Container's env before it hides.
const NAMES_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

/// What the Store held Sealed, as of one read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Names {
    Unread,
    /// A read failed: nothing is shown until a later read lands.
    Failed,
    /// The Store owns no such Namespace: every env value is scrubbed.
    Every,
    Only(BTreeSet<String>),
}

impl Names {
    /// The names a caller holds: `None` means the Store owns no such Namespace.
    pub(crate) fn held(names: Option<&BTreeSet<String>>) -> Self {
        names.map_or(Self::Every, |names| Self::Only(names.clone()))
    }

    /// Whether a Container env value under `name` is scrubbed.
    fn admits(&self, name: &str) -> bool {
        match self {
            Self::Every => true,
            Self::Only(names) => names.contains(name),
            Self::Unread | Self::Failed => false,
        }
    }
}

/// The values of a Container's `env` that `names` seal.
pub(crate) fn values_under(
    env: impl IntoIterator<Item = (String, String)>,
    names: &Names,
) -> SealedValues {
    env.into_iter()
        .filter(|(name, _)| names.admits(name))
        .collect()
}

#[derive(Debug)]
struct Read {
    names: Names,
    /// The `begin` that produced `names`. A read only lands over an older one.
    generation: u64,
}

/// The Sealed names one log request scrubs under, shared by its readers. Knows none until its
/// first read lands; until then readers wait, and while the newest read failed they hide.
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

/// Whether a Container's lines are shown, and under which forms, once no more names or env
/// are being waited for.
enum Settled {
    Show(Arc<SealedForms>),
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
        deadline: Instant,
        cancel: &CancellationToken,
    ) -> Settled {
        let mut changes = self.shared.read.subscribe();
        if matches!(changes.borrow().names, Names::Unread) {
            self.want();
        }
        let env = tokio::select! {
            () = cancel.cancelled() => return Settled::Hide,
            env = tokio::time::timeout_at(deadline, env.clone()) => env.ok(),
        };
        loop {
            let read = Arc::clone(&changes.borrow_and_update());
            if let Some(settled) = verdict(&read, env.as_deref()) {
                return settled;
            }
            self.want();
            tokio::select! {
                () = cancel.cancelled() => return Settled::Hide,
                changed = tokio::time::timeout_at(deadline, changes.changed()) => {
                    if !matches!(changed, Ok(Ok(()))) {
                        return Settled::Hide;
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

/// The fail-closed table: what the lines show, or None while names or the env are still to
/// come. `env` is None until the Container's env has been read.
fn verdict(read: &Read, env: Option<&Env>) -> Option<Settled> {
    match &read.names {
        Names::Failed => Some(Settled::Hide),
        Names::Unread => None,
        Names::Every | Names::Only(_) => match env {
            None => None,
            // A Deploy may have Sealed a name and started this Container after these names were read.
            Some(env) if read.generation <= env.after => None,
            Some(Env { values: None, .. }) => Some(Settled::Hide),
            Some(env) => Some(Settled::Show(env.forms(read))),
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
        let values = values_under(
            self.values
                .iter()
                .flatten()
                .map(|(name, value)| (name.clone(), value.clone())),
            &read.names,
        );
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
        names.settle(&env, deadline, &self.inner.cancel).await;
        let env = env.await;
        Ok(ScrubbedLogStream {
            raw: self.container_logs(input).await?,
            names: names.clone(),
            env,
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

    fn verdict(&self) -> Settled {
        verdict(&self.names.read(), Some(&self.env)).unwrap_or(Settled::Hide)
    }
}

impl LiveRead {
    fn take(&mut self, entry: LogEntry, verdict: Settled) {
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
            Settled::Show(forms) => {
                let shown = self.scrub.push(channel, &bytes, &forms);
                self.show(channel, shown, entry.metadata, entry.timestamp_unix_nanos);
            }
            Settled::Hide => {
                self.scrub.discard(channel);
                self.hide(channel, entry.metadata, entry.timestamp_unix_nanos);
            }
        }
    }

    /// Show what is held, scrubbed, and end the stream.
    fn end(&mut self, verdict: Settled) {
        self.ended = true;
        let scrub = std::mem::take(&mut self.scrub);
        let Some((source, timestamp)) = self.last.take() else {
            return;
        };
        match verdict {
            Settled::Show(forms) => {
                for (channel, shown) in scrub.finish(&forms) {
                    self.show(channel, shown, source.clone(), timestamp);
                }
            }
            Settled::Hide => {
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

#[cfg(test)]
#[path = "sealed_tests.rs"]
mod tests;
