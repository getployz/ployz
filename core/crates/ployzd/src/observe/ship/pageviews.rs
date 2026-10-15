use std::{
    collections::BTreeMap,
    fs, io,
    path::PathBuf,
    sync::{Arc, Mutex, PoisonError},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use serde::Serialize;
use tokio::sync::Notify;

use super::{
    CAP, Held, Ledger, PAGEVIEWS, StoreRoot, Stream, create_store_dir, entry_name, remove_entry,
    seqs,
};

const SEGMENT_SUFFIX: &str = ".jsonl";
const SEGMENT_ROWS: usize = 100;
const SEGMENT_BYTES: usize = 16 * 1024;
/// Cloud answers 413 past 256 KiB; a row is at most a few KiB, so a batch
/// that stops adding segments here stays well under it.
const BATCH_BYTES: usize = 192 * 1024;
const SEGMENT_AGE: Duration = Duration::from_secs(1);
pub(in crate::observe) const ROW_CAP: usize = 50_000;

pub(in crate::observe) struct PageviewOutbox {
    dir: PathBuf,
    state: Mutex<State>,
    wake: Notify,
}

struct State {
    ledger: Ledger,
    segments: BTreeMap<u64, Segment>,
    rows: usize,
    buffer: Buffer,
}

#[derive(Clone, Copy)]
struct Segment {
    rows: usize,
    bytes: usize,
}

#[derive(Default)]
struct Buffer {
    lines: Vec<u8>,
    rows: usize,
    since: Option<Instant>,
}

impl PageviewOutbox {
    pub(in crate::observe) fn open(store: &StoreRoot) -> io::Result<Arc<Self>> {
        let dir = store.pageviews_outbox();
        create_store_dir(dir.parent().unwrap_or(&dir))?;
        create_store_dir(&dir)?;
        let mut segments = BTreeMap::new();
        for seq in seqs(&dir, SEGMENT_SUFFIX)? {
            match fs::read(dir.join(entry_name(seq, SEGMENT_SUFFIX))) {
                Ok(bytes) => {
                    let rows = lines(&bytes).count();
                    segments.insert(
                        seq,
                        Segment {
                            rows,
                            bytes: bytes.len(),
                        },
                    );
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        let newest = segments.keys().next_back().copied().unwrap_or(0);
        let state = State {
            ledger: Ledger::open(&dir, newest),
            rows: segments.values().map(|segment| segment.rows).sum(),
            segments,
            buffer: Buffer::default(),
        };
        Ok(Arc::new(Self {
            dir,
            state: Mutex::new(state),
            wake: Notify::new(),
        }))
    }

    pub(in crate::observe) fn push(&self, row: &impl Serialize) -> io::Result<()> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        serde_json::to_writer(&mut state.buffer.lines, row).map_err(io::Error::other)?;
        state.buffer.lines.push(b'\n');
        state.buffer.rows += 1;
        state.buffer.since.get_or_insert_with(Instant::now);
        if state.buffer.rows >= SEGMENT_ROWS || state.buffer.lines.len() >= SEGMENT_BYTES {
            self.flush(&mut state)?;
        }
        Ok(())
    }

    pub(in crate::observe) fn tick(&self) -> io::Result<()> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state
            .buffer
            .since
            .is_some_and(|since| since.elapsed() >= SEGMENT_AGE)
        {
            self.flush(&mut state)?;
        }
        Ok(())
    }

    fn flush(&self, state: &mut State) -> io::Result<()> {
        let buffer = std::mem::take(&mut state.buffer);
        if buffer.rows == 0 {
            return Ok(());
        }
        let seq = state.ledger.take_seq(&self.dir, clock_nanos())?;
        crate::filesystem::atomic_write(
            &self.dir.join(entry_name(seq, SEGMENT_SUFFIX)),
            &buffer.lines,
            0o600,
        )?;
        state.segments.insert(
            seq,
            Segment {
                rows: buffer.rows,
                bytes: buffer.lines.len(),
            },
        );
        state.rows += buffer.rows;
        let mut dropped = 0;
        while state.rows > ROW_CAP {
            let Some((oldest, segment)) = state.segments.pop_first() else {
                break;
            };
            remove_entry(&self.dir, oldest, SEGMENT_SUFFIX)?;
            state.rows -= segment.rows;
            dropped += segment.rows;
        }
        if dropped > 0 {
            state.ledger.count_dropped(&self.dir, dropped)?;
            tracing::debug!(
                dropped,
                "the pageviews outbox is full; dropping its oldest rows"
            );
        }
        self.wake.notify_one();
        Ok(())
    }

    #[cfg(test)]
    pub(in crate::observe) fn rows(&self) -> Vec<serde_json::Value> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        self.flush(&mut state).unwrap();
        state
            .segments
            .keys()
            .flat_map(|seq| {
                let bytes = fs::read(self.dir.join(entry_name(*seq, SEGMENT_SUFFIX))).unwrap();
                lines(&bytes)
                    .map(|line| serde_json::from_slice(line).unwrap())
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    #[cfg(test)]
    pub(in crate::observe) fn held_rows(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .rows
    }
}

impl Stream for PageviewOutbox {
    const NAME: &'static str = PAGEVIEWS;

    fn next(&self) -> io::Result<Option<Held>> {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let mut taken = Vec::new();
        let mut rows = 0;
        let mut bytes = 0;
        for (seq, segment) in &state.segments {
            if !taken.is_empty()
                && (rows + segment.rows > CAP || bytes + segment.bytes > BATCH_BYTES)
            {
                break;
            }
            taken.push(*seq);
            rows += segment.rows;
            bytes += segment.bytes;
        }
        let Some(last) = taken.last() else {
            return Ok(None);
        };
        let mut body = format!(
            r#"{{"stream":"{PAGEVIEWS}","seq":"{last}","dropped":{},"events":["#,
            state.ledger.dropped
        )
        .into_bytes();
        body.reserve(bytes);
        let mut first = true;
        for seq in &taken {
            let bytes = match fs::read(self.dir.join(entry_name(*seq, SEGMENT_SUFFIX))) {
                Ok(bytes) => bytes,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error),
            };
            for line in lines(&bytes) {
                if !first {
                    body.push(b',');
                }
                first = false;
                body.extend_from_slice(line);
            }
        }
        body.extend_from_slice(b"]}");
        Ok(Some(Held { body, seqs: taken }))
    }

    fn ack(&self, seqs: &[u64]) -> io::Result<()> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        for seq in seqs {
            remove_entry(&self.dir, *seq, SEGMENT_SUFFIX)?;
            if let Some(segment) = state.segments.remove(seq) {
                state.rows -= segment.rows;
            }
        }
        Ok(())
    }

    fn wake(&self) -> &Notify {
        &self.wake
    }
}

fn lines(bytes: &[u8]) -> impl Iterator<Item = &[u8]> {
    bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
}

fn clock_nanos() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX)
        })
}

#[cfg(test)]
pub(in crate::observe) fn dir_has(dir: &std::path::Path, needle: &[u8]) -> bool {
    fs::read_dir(dir).unwrap().any(|entry| {
        fs::read(entry.unwrap().path())
            .unwrap()
            .windows(needle.len())
            .any(|window| window == needle)
    })
}

#[cfg(test)]
#[expect(
    clippy::indexing_slicing,
    reason = "segment and JSON fixture assertions"
)]
mod tests {
    use super::*;
    use crate::test_dir::TestDir;

    fn store() -> (TestDir, StoreRoot) {
        let root = TestDir::new("ployzd-observe-pageviews-outbox");
        fs::create_dir_all(&root.0).unwrap();
        let store = StoreRoot::under(&root.0);
        store.prepare().unwrap();
        (root, store)
    }

    fn push(outbox: &PageviewOutbox, rows: std::ops::Range<usize>) {
        for n in rows {
            outbox
                .push(&serde_json::json!({ "kind": "pageview", "n": n }))
                .unwrap();
        }
    }

    fn segments(store: &StoreRoot) -> Vec<u64> {
        seqs(&store.pageviews_outbox(), SEGMENT_SUFFIX).unwrap()
    }

    fn batch(outbox: &PageviewOutbox) -> (serde_json::Value, Vec<u64>) {
        let held = outbox.next().unwrap().unwrap();
        (serde_json::from_slice(&held.body).unwrap(), held.seqs)
    }

    #[test]
    fn a_segment_is_written_at_100_rows_or_after_a_second() {
        let (_root, store) = store();
        let outbox = PageviewOutbox::open(&store).unwrap();
        push(&outbox, 0..99);
        outbox.tick().unwrap();
        assert!(segments(&store).is_empty());
        assert!(outbox.next().unwrap().is_none());
        push(&outbox, 99..100);
        assert_eq!(segments(&store).len(), 1);

        push(&outbox, 100..101);
        outbox.tick().unwrap();
        assert_eq!(segments(&store).len(), 1);
        outbox.state.lock().unwrap().buffer.since = Instant::now().checked_sub(SEGMENT_AGE);
        outbox.tick().unwrap();
        assert_eq!(segments(&store).len(), 2);
        assert_eq!(outbox.held_rows(), 101);
        let mode = fs::metadata(
            store
                .pageviews_outbox()
                .join(entry_name(segments(&store)[0], SEGMENT_SUFFIX)),
        )
        .unwrap()
        .permissions();
        assert_eq!(
            std::os::unix::fs::PermissionsExt::mode(&mode) & 0o777,
            0o600
        );
    }

    #[test]
    fn a_batch_takes_whole_oldest_segments_up_to_1000_rows_and_acks_them_whole() {
        let (_root, store) = store();
        let outbox = PageviewOutbox::open(&store).unwrap();
        push(&outbox, 0..1150);
        outbox.state.lock().unwrap().buffer.since = Instant::now().checked_sub(SEGMENT_AGE);
        outbox.tick().unwrap();
        let written = segments(&store);
        assert_eq!(written.len(), 12);

        let (body, seqs) = batch(&outbox);
        assert_eq!(seqs, written[..10]);
        assert_eq!(body["stream"], "pageviews");
        assert_eq!(body["seq"], written[9].to_string());
        assert_eq!(body["dropped"], 0);
        let events = body["events"].as_array().unwrap();
        assert_eq!(events.len(), 1000);
        assert_eq!(events[999]["n"], 999);

        outbox.ack(&seqs).unwrap();
        assert_eq!(segments(&store), written[10..]);
        let (body, seqs) = batch(&outbox);
        assert_eq!(seqs, written[10..]);
        assert_eq!(body["events"].as_array().unwrap().len(), 150);
    }

    #[test]
    fn a_full_outbox_drops_its_oldest_segments_and_counts_their_rows_across_a_restart() {
        let (_root, store) = store();
        let outbox = PageviewOutbox::open(&store).unwrap();
        push(&outbox, 0..ROW_CAP + 250);
        assert_eq!(outbox.held_rows(), ROW_CAP);
        let (body, _) = batch(&outbox);
        assert_eq!(body["dropped"], 200);
        assert_eq!(body["events"][0]["n"], 200);
        drop(outbox);

        let reopened = PageviewOutbox::open(&store).unwrap();
        assert_eq!(reopened.held_rows(), ROW_CAP);
        assert_eq!(batch(&reopened).0["dropped"], 200);
        push(&reopened, 0..100);
        assert_eq!(batch(&reopened).0["dropped"], 300);
    }

    fn pageview(path: &str, query_value: &str) -> serde_json::Value {
        serde_json::json!({
            "kind": "pageview",
            "ts": 1_792_026_000_123_i64,
            "day": "2026-10-15",
            "namespace": "acme-production",
            "host": "www.acme-widgets.example.com",
            "path": path,
            "query": {
                "utm_source": query_value, "utm_medium": query_value, "utm_campaign": query_value,
                "utm_term": query_value, "utm_content": query_value, "ref": query_value,
            },
            "referrer_host": "news.ycombinator.com",
            "language": "en",
            "visitor": "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08",
            "flags": ["no_sec_fetch", "click_id"],
        })
    }

    fn batches_until_empty(outbox: &PageviewOutbox) -> Vec<usize> {
        let mut sizes = Vec::new();
        let mut events = 0;
        while let Some(held) = outbox.next().unwrap() {
            let body: serde_json::Value = serde_json::from_slice(&held.body).unwrap();
            events += body["events"].as_array().unwrap().len();
            sizes.push(held.body.len());
            outbox.ack(&held.seqs).unwrap();
        }
        assert_eq!(events, 1000);
        sizes
    }

    #[test]
    fn a_thousand_realistic_rows_ship_in_batches_under_clouds_256_kib_cap() {
        let (_root, store) = store();
        let outbox = PageviewOutbox::open(&store).unwrap();
        for n in 0..1000 {
            outbox
                .push(&pageview(
                    &format!("/blog/2026/10/how-we-ship-{n}"),
                    "newsletter",
                ))
                .unwrap();
        }
        outbox.state.lock().unwrap().buffer.since = Instant::now().checked_sub(SEGMENT_AGE);
        outbox.tick().unwrap();
        let sizes = batches_until_empty(&outbox);
        assert!(sizes.iter().sum::<usize>() > 262_144, "{sizes:?}");
        assert!(
            sizes.iter().all(|size| *size <= BATCH_BYTES + 1024),
            "{sizes:?}"
        );
    }

    #[test]
    fn the_largest_rows_the_harvester_writes_still_ship_under_the_cap() {
        let (_root, store) = store();
        let outbox = PageviewOutbox::open(&store).unwrap();
        let path = "\u{1}".repeat(1024);
        let value = "\u{1}".repeat(256);
        for _ in 0..1000 {
            outbox.push(&pageview(&path, &value)).unwrap();
        }
        outbox.state.lock().unwrap().buffer.since = Instant::now().checked_sub(SEGMENT_AGE);
        outbox.tick().unwrap();
        let sizes = batches_until_empty(&outbox);
        assert!(sizes.iter().all(|size| *size < 262_144), "{sizes:?}");
    }

    #[test]
    fn segment_seqs_keep_increasing_across_a_restart() {
        let (_root, store) = store();
        let outbox = PageviewOutbox::open(&store).unwrap();
        push(&outbox, 0..100);
        let (_, seqs) = batch(&outbox);
        outbox.ack(&seqs).unwrap();
        drop(outbox);
        let reopened = PageviewOutbox::open(&store).unwrap();
        push(&reopened, 0..100);
        assert!(segments(&store)[0] > seqs[0]);
    }
}
