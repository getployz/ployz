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
const SEGMENT_AGE: Duration = Duration::from_secs(1);
pub(in crate::observe) const ROW_CAP: usize = 50_000;

pub(in crate::observe) struct PageviewOutbox {
    dir: PathBuf,
    state: Mutex<State>,
    wake: Notify,
}

struct State {
    ledger: Ledger,
    segments: BTreeMap<u64, usize>,
    rows: usize,
    buffer: Buffer,
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
                    segments.insert(seq, lines(&bytes).count());
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        let newest = segments.keys().next_back().copied().unwrap_or(0);
        let state = State {
            ledger: Ledger::open(&dir, newest),
            rows: segments.values().sum(),
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
        if state.buffer.rows >= SEGMENT_ROWS {
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
        state.segments.insert(seq, buffer.rows);
        state.rows += buffer.rows;
        let mut dropped = 0;
        while state.rows > ROW_CAP {
            let Some((oldest, rows)) = state.segments.pop_first() else {
                break;
            };
            remove_entry(&self.dir, oldest, SEGMENT_SUFFIX)?;
            state.rows -= rows;
            dropped += rows;
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

    fn remove(&self, seqs: &[u64], refused: bool) -> io::Result<()> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let mut removed = 0;
        for seq in seqs {
            remove_entry(&self.dir, *seq, SEGMENT_SUFFIX)?;
            if let Some(rows) = state.segments.remove(seq) {
                state.rows -= rows;
                removed += rows;
            }
        }
        if refused {
            state.ledger.count_dropped(&self.dir, removed)?;
        }
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
        for (seq, count) in &state.segments {
            if !taken.is_empty() && rows + count > CAP {
                break;
            }
            taken.push(*seq);
            rows += count;
        }
        let Some(last) = taken.last() else {
            return Ok(None);
        };
        let mut body = format!(
            r#"{{"stream":"{PAGEVIEWS}","seq":"{last}","dropped":{},"events":["#,
            state.ledger.dropped
        )
        .into_bytes();
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
        self.remove(seqs, false)
    }

    fn refuse(&self, seqs: &[u64]) -> io::Result<()> {
        self.remove(seqs, true)
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
        let (body, seqs) = batch(&reopened);
        assert_eq!(body["dropped"], 200);
        reopened.refuse(&seqs).unwrap();
        assert_eq!(batch(&reopened).0["dropped"], 1200);
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
