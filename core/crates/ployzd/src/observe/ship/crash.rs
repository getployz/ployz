//! The crash stream: a Container's exit queued under a seq that survives a restart, and
//! shipped to Cloud oldest first.

use std::{
    fs, io,
    path::PathBuf,
    sync::{Arc, Mutex, PoisonError},
};

use ployz_core::metrics::MetricMarker;
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;

use super::{
    push::{Held, Stream},
    store::{Ledger, entry_name, exists, remove_entry, seqs},
};
use crate::observe::layout::{StoreRoot, create_store_dir};

pub(in crate::observe) const CAP: usize = 1000;
const BATCH: usize = 100;
const CRASH: &str = "crash";
const RESERVED: &str = ".reserved";

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub(in crate::observe) struct Entry {
    #[serde(with = "decimal")]
    pub(in crate::observe) seq: u64,
    pub(in crate::observe) marker: MetricMarker,
}

#[derive(Debug, Serialize)]
struct Batch<'a> {
    stream: &'static str,
    #[serde(with = "decimal")]
    seq: u64,
    dropped: u64,
    events: &'a [Entry],
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
        let seqs = seqs(&self.dir, "")?;
        let queued = seqs.binary_search(&seq).is_ok();
        let reserved = self.dir.join(entry_name(seq, RESERVED));
        if !queued && !exists(&reserved)? {
            return Ok(false);
        }
        let excess = (seqs.len() + usize::from(!queued)).saturating_sub(CAP);
        if excess > 0 {
            for oldest in seqs.iter().filter(|queued| **queued != seq).take(excess) {
                remove_entry(&self.dir, *oldest, "")?;
            }
            ledger.count_dropped(&self.dir, excess)?;
            tracing::warn!(
                dropped = excess,
                "the crash outbox is full; dropping its oldest entries"
            );
        }
        let entry = Entry { seq, marker };
        let json = serde_json::to_vec(&entry).map_err(io::Error::other)?;
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

    /// Says whether `seq` is reserved and not yet queued.
    pub fn is_reserved(&self, seq: u64) -> io::Result<bool> {
        exists(&self.dir.join(entry_name(seq, RESERVED)))
    }

    /// Releases the reserved seqs not in `keep`, which nothing will queue.
    pub fn release_reserved(&self, keep: &[u64]) -> io::Result<()> {
        let _ledger = self.ledger.lock().unwrap_or_else(PoisonError::into_inner);
        for seq in seqs(&self.dir, RESERVED)? {
            if !keep.contains(&seq) {
                remove_entry(&self.dir, seq, RESERVED)?;
            }
        }
        Ok(())
    }

    pub(in crate::observe) fn oldest(&self, limit: usize) -> io::Result<Vec<Entry>> {
        let mut ledger = self.ledger.lock().unwrap_or_else(PoisonError::into_inner);
        let mut entries = Vec::new();
        for seq in seqs(&self.dir, "")? {
            if entries.len() == limit {
                break;
            }
            let path = self.dir.join(entry_name(seq, ""));
            let bytes = match fs::read(&path) {
                Ok(bytes) => bytes,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error),
            };
            match serde_json::from_slice::<Entry>(&bytes) {
                Ok(entry) => entries.push(entry),
                Err(error) => {
                    tracing::warn!(path = %path.display(), %error, "dropping an unreadable crash outbox entry");
                    remove_entry(&self.dir, seq, "")?;
                    ledger.count_dropped(&self.dir, 1)?;
                }
            }
        }
        Ok(entries)
    }

    fn ledger(&self) -> (u64, u64) {
        let ledger = self.ledger.lock().unwrap_or_else(PoisonError::into_inner);
        (ledger.last_seq, ledger.dropped)
    }

    fn batch(&self, seq: u64, events: &[Entry]) -> io::Result<Held> {
        let (_, dropped) = self.ledger();
        let batch = Batch {
            stream: CRASH,
            seq,
            dropped,
            events,
        };
        Ok(Held {
            body: serde_json::to_vec(&batch).map_err(io::Error::other)?,
            seqs: events.iter().map(|entry| entry.seq).collect(),
        })
    }
}

impl Stream for Outbox {
    const NAME: &'static str = CRASH;

    fn next(&self) -> io::Result<Option<Held>> {
        let events = self.oldest(BATCH)?;
        let Some(seq) = events.last().map(|entry| entry.seq) else {
            return Ok(None);
        };
        self.batch(seq, &events).map(Some)
    }

    fn ack(&self, seqs: &[u64]) -> io::Result<()> {
        let _ledger = self.ledger.lock().unwrap_or_else(PoisonError::into_inner);
        for seq in seqs {
            remove_entry(&self.dir, *seq, "")?;
        }
        Ok(())
    }

    fn refuse(&self, seqs: &[u64]) -> io::Result<()> {
        let mut ledger = self.ledger.lock().unwrap_or_else(PoisonError::into_inner);
        let mut removed = 0;
        for seq in seqs {
            removed += usize::from(remove_entry(&self.dir, *seq, "")?);
        }
        if removed == 0 {
            return Ok(());
        }
        ledger.count_dropped(&self.dir, removed)
    }

    fn wake(&self) -> &Notify {
        &self.wake
    }

    fn salt_refresh(&self) -> io::Result<Option<Held>> {
        let (last_seq, _) = self.ledger();
        self.batch(last_seq, &[]).map(Some)
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
