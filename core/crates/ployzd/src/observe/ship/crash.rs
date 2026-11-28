//! Crash events: a Container's exit queued under a seq that survives a restart, rendered as
//! an OTLP log record and posted to the Collector's local receiver, oldest first. The
//! Collector carries it to Cloud with the bearer token, so a crash is posted again after
//! any failure whose outcome is unknown and Cloud dedupes by `ployz.event.id`.

use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, PoisonError},
    time::Duration,
};

use ployz_core::metrics::{MarkerKind, MetricMarker};
use reqwest::header::CONTENT_TYPE;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::Notify;

use super::store::{self, File, Ledger, entry_name, exists, seqs};
use crate::{
    backoff::Doubling,
    observe::layout::{StoreRoot, create_store_dir},
    written::{
        Content, Digest, Encoding, Inspection, Json, Removal, Verdict, Widest, Written, inspect,
        keep_aside, kept_index, remove_written, to_json,
    },
};

pub(in crate::observe) const CAP: usize = 1000;
const BATCH: usize = 100;
const RESERVED: &str = ".reserved";
const TICK: Duration = Duration::from_secs(10);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const BACKOFF_FLOOR: Duration = Duration::from_secs(1);
const BACKOFF_CAP: Duration = Duration::from_secs(30);
const EVENT_NAME: &str = "ployz.container.exit";
const SEVERITY_ERROR: u8 = 17;

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub(in crate::observe) struct Entry {
    #[serde(with = "decimal")]
    pub(in crate::observe) seq: u64,
    pub(in crate::observe) marker: MetricMarker,
}

/// The file `name` is in the crash outbox: an entry holds its JSON under its own seq, and a
/// reservation holds nothing until the entry is written into it. Only a crash is queued, so an
/// entry's marker is one.
pub(in crate::observe) fn file(name: &str) -> Option<Written> {
    let (file, staged) = store::file(name, &["", RESERVED])?;
    Some(match file {
        File::Counter => Written::new(Content::U64, staged),
        File::Entry { seq, suffix } => {
            let entry = Written::new(
                Content::Json(Box::new(Json {
                    identity: |entry: &Entry| entry.seq,
                    named: seq,
                    retired: &[],
                    valid: |entry: &Entry| entry.marker.is_crash(),
                    // An entry is only ever rewritten under its own seq.
                    widest: Entry::clone,
                    encoding: Encoding::Compact,
                })),
                staged,
            );
            if suffix == RESERVED {
                entry.or_empty()
            } else {
                entry
            }
        }
    })
}

/// Whether `name` is an entry [`keep_aside`] kept aside, too large to post.
pub(in crate::observe) fn kept(name: &str) -> bool {
    name.split_once(".kept-").is_some_and(|(entry, _)| {
        matches!(store::file(entry, &[""]), Some((File::Entry { .. }, false)))
            && kept_index(name.as_ref(), &format!("{entry}.kept-")).is_some()
    })
}

/// Whether a crash entry for `marker` fits the store under any seq it is queued under.
pub(in crate::observe) fn fits(marker: &MetricMarker) -> bool {
    let widest = Entry {
        seq: u64::MAX,
        marker: marker.clone(),
    };
    to_json(&widest, Widest::AsWritten).is_ok()
}

fn refuse(ledger: &mut Ledger, dir: &Path) -> io::Result<()> {
    ledger.count_dropped(dir, 1)
}

/// Removes the outbox file `name` when it holds what the outbox writes there and, given
/// `selected`, still the bytes selected. Anything else stays, logged.
fn remove(dir: &Path, name: &str, selected: Option<Digest>) -> io::Result<Removal> {
    remove_written(&dir.join(name), file(name).as_ref(), selected)
}

/// An entry as the sender selected it: the bytes an ack may remove.
#[derive(Debug)]
pub(in crate::observe) struct Queued {
    pub(in crate::observe) entry: Entry,
    digest: Digest,
}

/// The entries queued in `dir`, oldest first: the seqs and the digest of each file the outbox
/// wrote, the only ones it posts, evicts or acks. An entry too large for this release, as a
/// release before the cap could leave, is kept aside and logged. Anything else stays, logged.
fn owned(dir: &Path) -> io::Result<Vec<(u64, Inspection)>> {
    let mut owned = Vec::new();
    for seq in seqs(dir, "")? {
        let name = entry_name(seq, "");
        let path = dir.join(&name);
        let inspection = inspect(&path, file(&name).as_ref())?;
        match inspection.verdict() {
            Some(Verdict::Ours) => owned.push((seq, inspection)),
            Some(Verdict::Oversized) => {
                tracing::warn!(path = %path.display(), "a crash outbox entry is too large to post");
                keep_aside(&path, &inspection)?;
            }
            Some(Verdict::Foreign) => {
                tracing::warn!(path = %path.display(), "skipping a crash outbox entry Ployz did not write");
            }
            None => {}
        }
    }
    Ok(owned)
}

pub struct Outbox {
    dir: PathBuf,
    ledger: Mutex<Ledger>,
    wake: Notify,
}

impl Outbox {
    pub fn open(store: &StoreRoot) -> io::Result<Arc<Self>> {
        let dir = store.crash_outbox();
        create_store_dir(dir.parent().unwrap_or(&dir))?;
        create_store_dir(&dir)?;
        let newest = seqs(&dir, "")?.last().copied().unwrap_or(0);
        Ok(Arc::new(Self {
            ledger: Mutex::new(Ledger::open(&dir, newest)),
            dir,
            wake: Notify::new(),
        }))
    }

    #[cfg(test)]
    pub fn push(&self, marker: MetricMarker) -> io::Result<()> {
        let seq = self.reserve(marker.ts)?;
        self.push_at(seq, marker).map(drop)
    }

    /// Hands out the seq a crash is queued under, held by a `.reserved`
    /// file until [`Outbox::push_at`] queues it.
    pub fn reserve(&self, ts: i64) -> io::Result<u64> {
        let mut ledger = self.ledger.lock().unwrap_or_else(PoisonError::into_inner);
        let seq = ledger.take_seq(&self.dir, u64::try_from(ts).unwrap_or(0))?;
        crate::filesystem::atomic_write(&self.dir.join(entry_name(seq, RESERVED)), b"", 0o600)?;
        Ok(seq)
    }

    /// Queues `marker` under a seq [`Outbox::reserve`] handed out, and says
    /// whether it did. A seq still queued is rewritten. One no longer
    /// reserved or queued has shipped or been dropped, and queuing it again
    /// would only take a slot from a newer crash.
    pub fn push_at(&self, seq: u64, marker: MetricMarker) -> io::Result<bool> {
        let mut ledger = self.ledger.lock().unwrap_or_else(PoisonError::into_inner);
        let queued = seqs(&self.dir, "")?.binary_search(&seq).is_ok();
        let reserved = self.dir.join(entry_name(seq, RESERVED));
        if !queued && !exists(&reserved)? {
            return Ok(false);
        }
        let entry = Entry { seq, marker };
        let json = match to_json(&entry, Widest::AsWritten) {
            Err(error) if error.kind() == io::ErrorKind::FileTooLarge => {
                tracing::warn!(%error, "a crash is too large for the crash outbox; dropping it");
                if !queued {
                    remove(&self.dir, &entry_name(seq, RESERVED), None)?;
                }
                refuse(&mut ledger, &self.dir)?;
                return Ok(false);
            }
            json => json?,
        };
        // Only entries the outbox wrote take a slot, and only they are evicted and counted.
        let others: Vec<_> = owned(&self.dir)?
            .into_iter()
            .filter(|(owned, _)| *owned != seq)
            .collect();
        let excess = (others.len() + 1).saturating_sub(CAP);
        let mut evicted = 0;
        for (oldest, inspection) in others.iter().take(excess) {
            let removal = remove(&self.dir, &entry_name(*oldest, ""), inspection.digest())?;
            if removal == Removal::Removed {
                evicted += 1;
            }
        }
        if evicted > 0 {
            ledger.count_dropped(&self.dir, evicted)?;
            tracing::warn!(
                dropped = evicted,
                "the crash outbox is full; dropping its oldest entries"
            );
        }
        if queued {
            crate::filesystem::atomic_write(&self.dir.join(entry_name(seq, "")), &json, 0o600)?;
        } else {
            crate::filesystem::atomic_write(&reserved, &json, 0o600)?;
            fs::rename(&reserved, self.dir.join(entry_name(seq, "")))?;
            fs::File::open(&self.dir)?.sync_all()?;
        }
        drop(ledger);
        self.wake.notify_one();
        Ok(true)
    }

    /// Counts a crash too large to queue as dropped.
    pub fn refuse(&self) -> io::Result<()> {
        let mut ledger = self.ledger.lock().unwrap_or_else(PoisonError::into_inner);
        refuse(&mut ledger, &self.dir)
    }

    /// Says whether `seq` is reserved and not yet queued.
    pub fn is_reserved(&self, seq: u64) -> io::Result<bool> {
        exists(&self.dir.join(entry_name(seq, RESERVED)))
    }

    /// Releases the reserved seqs not in `keep`, which nothing will queue.
    pub fn release_reserved(&self, keep: &[u64]) -> io::Result<()> {
        let _ledger = self.ledger.lock().unwrap_or_else(PoisonError::into_inner);
        for seq in seqs(&self.dir, RESERVED)? {
            if !keep.contains(&seq) {
                remove(&self.dir, &entry_name(seq, RESERVED), None)?;
            }
        }
        Ok(())
    }

    /// The oldest `limit` entries the outbox wrote, as selected for an ack.
    pub(in crate::observe) fn select(&self, limit: usize) -> io::Result<Vec<Queued>> {
        let _ledger = self.ledger.lock().unwrap_or_else(PoisonError::into_inner);
        let mut selected = Vec::new();
        for (_, inspection) in owned(&self.dir)?.into_iter().take(limit) {
            let Inspection::File { bytes, .. } = &inspection else {
                continue;
            };
            selected.push(Queued {
                entry: serde_json::from_slice(bytes).map_err(io::Error::other)?,
                digest: inspection.digest().unwrap_or_default(),
            });
        }
        Ok(selected)
    }

    #[cfg(test)]
    pub(in crate::observe) fn oldest(&self, limit: usize) -> io::Result<Vec<Entry>> {
        Ok(self
            .select(limit)?
            .into_iter()
            .map(|queued| queued.entry)
            .collect())
    }

    #[cfg(test)]
    pub(in crate::observe) fn dropped(&self) -> u64 {
        self.ledger
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .dropped
    }

    /// Removes the entries `posted` selected, each only while it still holds the bytes
    /// selected: one rewritten since stays to be posted again.
    pub(in crate::observe) fn ack(&self, posted: &[Queued]) -> io::Result<()> {
        let _ledger = self.ledger.lock().unwrap_or_else(PoisonError::into_inner);
        for queued in posted {
            remove(
                &self.dir,
                &entry_name(queued.entry.seq, ""),
                Some(queued.digest),
            )?;
        }
        Ok(())
    }

    /// Acks the entries now queued under `seqs`.
    #[cfg(test)]
    pub(in crate::observe) fn ack_seqs(&self, seqs: &[u64]) -> io::Result<()> {
        let posted: Vec<_> = self
            .select(CAP)?
            .into_iter()
            .filter(|queued| seqs.contains(&queued.entry.seq))
            .collect();
        self.ack(&posted)
    }
}

fn string(key: &str, value: &str) -> Value {
    json!({ "key": key, "value": { "stringValue": value } })
}

fn log_record(marker: &MetricMarker) -> Value {
    let (exit_code, oom) = match marker.kind {
        MarkerKind::Exit { exit_code, oom, .. } => (exit_code, oom),
        MarkerKind::Oom => (None, true),
        MarkerKind::Restart => (None, false),
    };
    let replica = &marker.replica;
    let body = match (exit_code, oom) {
        (Some(code), true) => format!("{replica} exited with code {code} (out of memory)"),
        (Some(code), false) => format!("{replica} exited with code {code}"),
        (None, true) => format!("{replica} was killed by the out-of-memory killer"),
        (None, false) => format!("{replica} exited"),
    };
    let ts = marker.ts.to_string();
    let mut attributes = vec![
        string("event.name", EVENT_NAME),
        string(
            "ployz.event.id",
            &format!("{}:{ts}", marker.container_id.as_str()),
        ),
        string("ployz.machine.id", marker.machine_id.as_str()),
        string("ployz.namespace", marker.service.namespace.as_str()),
        string("ployz.service.name", marker.service.name.as_str()),
        string("ployz.service.id", marker.service_id.as_str()),
        string("container.id", marker.container_id.as_str()),
        string("ployz.replica", replica),
    ];
    if let Some(deployment) = &marker.deployment_id {
        attributes.push(string("ployz.deployment.id", deployment));
    }
    if let Some(code) = exit_code {
        attributes.push(json!({
            "key": "ployz.exit.code",
            "value": { "intValue": code.to_string() },
        }));
    }
    attributes.push(json!({ "key": "ployz.exit.oom", "value": { "boolValue": oom } }));
    attributes.push(string(
        "ployz.exit.reason",
        if oom { "out_of_memory" } else { "exit_nonzero" },
    ));
    json!({
        "timeUnixNano": ts,
        "observedTimeUnixNano": ts,
        "severityNumber": SEVERITY_ERROR,
        "severityText": "ERROR",
        "body": { "stringValue": body },
        "eventName": EVENT_NAME,
        "attributes": attributes,
    })
}

fn render<'a>(entries: impl IntoIterator<Item = &'a Entry>) -> Vec<u8> {
    let records: Vec<Value> = entries
        .into_iter()
        .map(|entry| log_record(&entry.marker))
        .collect();
    let body = json!({ "resourceLogs": [{ "scopeLogs": [{ "logRecords": records }] }] });
    serde_json::to_vec(&body).expect("a JSON value serializes")
}

#[derive(Debug, PartialEq)]
enum Sent {
    Idle,
    Progress,
    Retry,
}

/// Posts queued crashes to the Collector's receiver. The Collector runs only on a Machine
/// with a push target, so without one the entries wait on disk.
pub(in crate::observe) struct Sender<F> {
    outbox: Arc<Outbox>,
    client: reqwest::Client,
    url: String,
    connected: F,
}

impl<F: Fn() -> bool> Sender<F> {
    pub(in crate::observe) fn new(
        outbox: Arc<Outbox>,
        receiver: &str,
        connected: F,
    ) -> reqwest::Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()?;
        Ok(Self {
            outbox,
            client,
            url: format!("http://{receiver}/v1/logs"),
            connected,
        })
    }

    pub(in crate::observe) async fn run(self) {
        let mut backoff = Doubling::between(BACKOFF_FLOOR, BACKOFF_CAP);
        loop {
            match self.send_once().await {
                Sent::Progress => backoff.reset(),
                Sent::Idle => {
                    backoff.reset();
                    tokio::select! {
                        () = self.outbox.wake.notified() => {}
                        () = tokio::time::sleep(TICK) => {}
                    }
                }
                Sent::Retry => tokio::time::sleep(backoff.take()).await,
            }
        }
    }

    async fn send_once(&self) -> Sent {
        if !(self.connected)() {
            return Sent::Idle;
        }
        let selected = match self.outbox.select(BATCH) {
            Ok(selected) if selected.is_empty() => return Sent::Idle,
            Ok(selected) => selected,
            Err(error) => {
                tracing::warn!(%error, "cannot read the crash outbox");
                return Sent::Retry;
            }
        };
        let response = self
            .client
            .post(&self.url)
            .header(CONTENT_TYPE, "application/json")
            .body(render(selected.iter().map(|queued| &queued.entry)))
            .send()
            .await;
        match response {
            Ok(response) if response.status().is_success() => match self.outbox.ack(&selected) {
                Ok(()) => Sent::Progress,
                Err(error) => {
                    tracing::warn!(%error, "cannot delete posted crash entries");
                    Sent::Retry
                }
            },
            Ok(response) => {
                tracing::warn!(status = %response.status(), "the Collector did not take crash events; keeping them");
                Sent::Retry
            }
            Err(error) => {
                tracing::debug!(%error, "cannot reach the Collector to post crash events");
                Sent::Retry
            }
        }
    }
}

/// A u64 as a decimal string: seqs are nanos, past what a JS number holds
/// exactly.
mod decimal {
    use serde::{Deserialize, Deserializer, Serializer, de::Error};

    pub fn serialize<S: Serializer>(value: &u64, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(value)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u64, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(D::Error::custom)
    }
}

#[cfg(test)]
#[path = "crash_tests.rs"]
mod tests;
