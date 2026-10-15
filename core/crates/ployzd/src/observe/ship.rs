//! Ships crash markers to Cloud.
//!
//! The harvester pushes each crash into a durable outbox on disk, and the
//! shipper sends the oldest entries as one batch, deleting them only once
//! Cloud acks. Nothing here blocks the harvester on the network: a push is a
//! small file write and a wake-up.

use std::{
    collections::hash_map::RandomState,
    fs,
    hash::{BuildHasher, Hasher},
    io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, PoisonError},
    time::Duration,
};

use ployz_core::metrics::MetricMarker;
use reqwest::{RequestBuilder, StatusCode, Url, header::RETRY_AFTER};
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;

use super::layout::{StoreRoot, create_store_dir};

pub(super) const CAP: usize = 1000;
const BATCH: usize = 100;
const LAST_SEQ_FILE: &str = "last_seq";
const DROPPED_FILE: &str = "dropped";
const STREAM: &str = "crash";
const TICK: Duration = Duration::from_secs(30);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const BACKOFF_FLOOR: Duration = Duration::from_secs(1);
const BACKOFF_CAP: Duration = Duration::from_secs(5 * 60);

/// Where crash batches go, read before every send. None until Cloud can
/// authenticate a Machine's push, so the outbox only buffers.
pub fn push_target() -> Option<Url> {
    None
}

/// Adds the Machine's credentials to a push. The one place auth lands.
fn authenticate(request: RequestBuilder) -> RequestBuilder {
    request
}

/// One crash waiting for Cloud: the file body on disk and an event on the
/// wire alike.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub(super) struct Entry {
    #[serde(with = "decimal")]
    pub(super) seq: u64,
    pub(super) marker: MetricMarker,
}

/// The JSON body of one push.
#[derive(Debug, Serialize)]
struct Batch<'a> {
    stream: &'static str,
    /// The newest seq in `events`, which Cloud keeps as its cursor.
    #[serde(with = "decimal")]
    seq: u64,
    dropped: u64,
    events: &'a [Entry],
}

/// Crash markers on disk, oldest first, at most [`CAP`] of them.
pub struct Outbox {
    dir: PathBuf,
    ledger: Mutex<Ledger>,
    wake: Notify,
}

/// The counters persisted beside the entries.
#[derive(Debug)]
struct Ledger {
    last_seq: u64,
    dropped: u64,
}

impl Outbox {
    /// Opens the store's crash outbox, creating it when missing.
    ///
    /// # Errors
    ///
    /// Returns an error when the outbox dir cannot be created or read.
    pub fn open(store: &StoreRoot) -> io::Result<Arc<Self>> {
        let dir = store.crash_outbox();
        create_store_dir(dir.parent().unwrap_or(&dir))?;
        create_store_dir(&dir)?;
        let newest = seqs(&dir)?.last().copied().unwrap_or(0);
        let ledger = Ledger {
            last_seq: read_counter(&dir.join(LAST_SEQ_FILE)).max(newest),
            dropped: read_counter(&dir.join(DROPPED_FILE)),
        };
        Ok(Arc::new(Self {
            dir,
            ledger: Mutex::new(ledger),
            wake: Notify::new(),
        }))
    }

    /// Writes a crash marker as the newest entry and wakes the shipper. A
    /// full outbox loses its oldest entry to make room.
    ///
    /// # Errors
    ///
    /// Returns an error when the entry or the counters cannot be written.
    pub fn push(&self, marker: MetricMarker) -> io::Result<()> {
        let mut ledger = self.ledger.lock().unwrap_or_else(PoisonError::into_inner);
        let seqs = seqs(&self.dir)?;
        let excess = (seqs.len() + 1).saturating_sub(CAP);
        if excess > 0 {
            for seq in seqs.iter().take(excess) {
                remove_entry(&self.dir, *seq)?;
            }
            self.count_dropped(&mut ledger, excess)?;
            tracing::warn!(
                dropped = excess,
                "the crash outbox is full; dropping its oldest entries"
            );
        }
        let seq = u64::try_from(marker.ts)
            .unwrap_or(0)
            .max(ledger.last_seq.saturating_add(1));
        let entry = Entry { seq, marker };
        let json = serde_json::to_vec(&entry).map_err(io::Error::other)?;
        crate::filesystem::atomic_write(&self.dir.join(entry_name(seq)), &json, 0o600)?;
        ledger.last_seq = seq;
        write_counter(&self.dir.join(LAST_SEQ_FILE), seq)?;
        drop(ledger);
        self.wake.notify_one();
        Ok(())
    }

    /// The oldest entries, at most `limit`. An entry that cannot be parsed
    /// is deleted and counted as dropped, so it never blocks the ones after.
    pub(super) fn oldest(&self, limit: usize) -> io::Result<Vec<Entry>> {
        let mut ledger = self.ledger.lock().unwrap_or_else(PoisonError::into_inner);
        let mut entries = Vec::new();
        for seq in seqs(&self.dir)? {
            if entries.len() == limit {
                break;
            }
            let path = self.dir.join(entry_name(seq));
            let bytes = match fs::read(&path) {
                Ok(bytes) => bytes,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error),
            };
            match serde_json::from_slice::<Entry>(&bytes) {
                Ok(entry) => entries.push(entry),
                Err(error) => {
                    tracing::warn!(path = %path.display(), %error, "dropping an unreadable crash outbox entry");
                    remove_entry(&self.dir, seq)?;
                    self.count_dropped(&mut ledger, 1)?;
                }
            }
        }
        Ok(entries)
    }

    /// Deletes entries Cloud took.
    fn ack(&self, entries: &[Entry]) -> io::Result<()> {
        let _ledger = self.ledger.lock().unwrap_or_else(PoisonError::into_inner);
        for entry in entries {
            remove_entry(&self.dir, entry.seq)?;
        }
        Ok(())
    }

    /// Deletes entries Cloud refused and counts them as dropped.
    fn refuse(&self, entries: &[Entry]) -> io::Result<()> {
        let mut ledger = self.ledger.lock().unwrap_or_else(PoisonError::into_inner);
        for entry in entries {
            remove_entry(&self.dir, entry.seq)?;
        }
        self.count_dropped(&mut ledger, entries.len())
    }

    fn dropped(&self) -> u64 {
        self.ledger
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .dropped
    }

    fn count_dropped(&self, ledger: &mut Ledger, count: usize) -> io::Result<()> {
        ledger.dropped = ledger
            .dropped
            .saturating_add(u64::try_from(count).unwrap_or(u64::MAX));
        write_counter(&self.dir.join(DROPPED_FILE), ledger.dropped)
    }
}

fn entry_name(seq: u64) -> String {
    format!("{seq:020}")
}

/// Entry seqs, oldest first. Temp files and counters are not entries.
fn seqs(dir: &Path) -> io::Result<Vec<u64>> {
    let mut seqs = Vec::new();
    for entry in fs::read_dir(dir)? {
        let name = entry?.file_name();
        let Some(name) = name.to_str() else { continue };
        if name.len() == 20
            && let Ok(seq) = name.parse()
        {
            seqs.push(seq);
        }
    }
    seqs.sort_unstable();
    Ok(seqs)
}

fn remove_entry(dir: &Path, seq: u64) -> io::Result<()> {
    match fs::remove_file(dir.join(entry_name(seq))) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

fn read_counter(path: &Path) -> u64 {
    fs::read_to_string(path)
        .ok()
        .and_then(|text| text.trim().parse().ok())
        .unwrap_or(0)
}

fn write_counter(path: &Path, value: u64) -> io::Result<()> {
    crate::filesystem::atomic_write(path, value.to_string().as_bytes(), 0o600)
}

/// What one send attempt came to.
#[derive(Debug, PartialEq)]
enum Shipped {
    /// Nothing waits, or there is nowhere to send it yet.
    Idle,
    /// A batch went or was refused; more may wait.
    Progress,
    /// Keep the batch and try again later, no sooner than Cloud asked.
    Retry { after: Option<Duration> },
}

/// Sends the outbox to Cloud, oldest first.
pub struct Shipper<T> {
    outbox: Arc<Outbox>,
    client: reqwest::Client,
    target: T,
}

impl<T: Fn() -> Option<Url>> Shipper<T> {
    /// # Errors
    ///
    /// Returns an error when the HTTP client cannot be built.
    pub fn new(outbox: Arc<Outbox>, target: T) -> reqwest::Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()?;
        Ok(Self {
            outbox,
            client,
            target,
        })
    }

    /// Ships until the process ends.
    pub async fn run(self) {
        let mut backoff = Backoff::default();
        loop {
            match self.ship_once().await {
                Shipped::Progress => backoff.reset(),
                Shipped::Idle => {
                    backoff.reset();
                    tokio::select! {
                        () = self.outbox.wake.notified() => {}
                        () = tokio::time::sleep(TICK) => {}
                    }
                }
                Shipped::Retry { after } => tokio::time::sleep(backoff.next(after)).await,
            }
        }
    }

    async fn ship_once(&self) -> Shipped {
        let Some(target) = (self.target)() else {
            return Shipped::Idle;
        };
        let events = match self.outbox.oldest(BATCH) {
            Ok(events) => events,
            Err(error) => {
                tracing::warn!(%error, "cannot read the crash outbox");
                return Shipped::Retry { after: None };
            }
        };
        let Some(seq) = events.last().map(|entry| entry.seq) else {
            return Shipped::Idle;
        };
        let batch = Batch {
            stream: STREAM,
            seq,
            dropped: self.outbox.dropped(),
            events: &events,
        };
        let response = match authenticate(self.client.post(target).json(&batch))
            .send()
            .await
        {
            Ok(response) => response,
            Err(error) => {
                tracing::debug!(%error, "cannot reach Cloud to ship crashes");
                return Shipped::Retry { after: None };
            }
        };
        let status = response.status();
        let settled = match status {
            status if status.is_success() => self.outbox.ack(&events),
            StatusCode::BAD_REQUEST
            | StatusCode::PAYLOAD_TOO_LARGE
            | StatusCode::UNPROCESSABLE_ENTITY => {
                tracing::warn!(%status, count = events.len(), "Cloud refused a crash batch; dropping it");
                self.outbox.refuse(&events)
            }
            status if status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error() => {
                return Shipped::Retry {
                    after: retry_after(&response),
                };
            }
            status => {
                tracing::debug!(%status, "Cloud did not take a crash batch; keeping it");
                return Shipped::Retry { after: None };
            }
        };
        match settled {
            Ok(()) => Shipped::Progress,
            Err(error) => {
                tracing::warn!(%error, "cannot delete shipped crash outbox entries");
                Shipped::Retry { after: None }
            }
        }
    }
}

/// `Retry-After` in seconds. The HTTP-date form is rare enough to ignore.
fn retry_after(response: &reqwest::Response) -> Option<Duration> {
    let seconds = response.headers().get(RETRY_AFTER)?.to_str().ok()?;
    seconds.trim().parse().ok().map(Duration::from_secs)
}

/// Exponential backoff with jitter, from [`BACKOFF_FLOOR`] to
/// [`BACKOFF_CAP`].
#[derive(Debug)]
struct Backoff {
    ceiling: Duration,
}

impl Default for Backoff {
    fn default() -> Self {
        Self {
            ceiling: BACKOFF_FLOOR,
        }
    }
}

impl Backoff {
    /// The wait before the next attempt: what Cloud asked for when it said,
    /// else a random point in the upper half of the current ceiling, which
    /// then doubles.
    fn next(&mut self, asked: Option<Duration>) -> Duration {
        let ceiling = self.ceiling;
        self.ceiling = (ceiling * 2).min(BACKOFF_CAP);
        if let Some(asked) = asked {
            return asked.min(BACKOFF_CAP);
        }
        let half = ceiling / 2;
        half + half.mul_f64(jitter())
    }

    fn reset(&mut self) {
        self.ceiling = BACKOFF_FLOOR;
    }
}

/// A random fraction in `[0, 1)`. Hashers std seeds randomly are enough to
/// spread Machines apart, without a dependency.
fn jitter() -> f64 {
    let bits = RandomState::new().build_hasher().finish() >> 11;
    #[expect(
        clippy::cast_precision_loss,
        reason = "53 bits fit an f64 mantissa exactly"
    )]
    let fraction = bits as f64 / (1_u64 << 53) as f64;
    fraction
}

/// A u64 as a decimal string: crash seqs are nanos, past what a JS number
/// holds exactly.
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
mod tests {
    use std::sync::{Arc, Mutex};

    use axum::{Json, Router, extract::State, http::StatusCode, routing::post};
    use ployz_core::{ContainerId, MachineId, QualifiedService, ServiceId, metrics::MarkerKind};

    use super::*;
    use crate::test_dir::TestDir;

    fn store() -> (TestDir, StoreRoot) {
        let root = TestDir::new("ployzd-observe-ship");
        fs::create_dir_all(&root.0).unwrap();
        let store = StoreRoot::under(&root.0);
        store.prepare().unwrap();
        (root, store)
    }

    fn crash(ts: i64) -> MetricMarker {
        MetricMarker {
            machine_id: MachineId::parse("a".repeat(32)).unwrap(),
            container_id: ContainerId::parse("b".repeat(64)).unwrap(),
            service_id: ServiceId::parse("c".repeat(32)).unwrap(),
            service: QualifiedService::parse("prod/web").unwrap(),
            deployment_id: Some("d1".into()),
            replica: "web-1".into(),
            ts,
            kind: MarkerKind::Exit {
                exit_code: Some(137),
                oom: true,
                stopped: false,
            },
        }
    }

    fn held(outbox: &Outbox) -> Vec<u64> {
        outbox
            .oldest(CAP)
            .unwrap()
            .into_iter()
            .map(|entry| entry.seq)
            .collect()
    }

    #[test]
    fn seqs_strictly_increase_across_a_restart_even_when_clocks_repeat() {
        let (_root, store) = store();
        let outbox = Outbox::open(&store).unwrap();
        outbox.push(crash(100)).unwrap();
        outbox.push(crash(100)).unwrap();
        outbox.push(crash(50)).unwrap();
        drop(outbox);
        let reopened = Outbox::open(&store).unwrap();
        reopened.ack(&reopened.oldest(CAP).unwrap()).unwrap();
        reopened.push(crash(90)).unwrap();
        assert_eq!(
            reopened.oldest(CAP).unwrap(),
            [Entry {
                seq: 103,
                marker: crash(90),
            }]
        );
    }

    #[test]
    fn a_full_outbox_drops_its_oldest_entry_and_counts_it_across_a_restart() {
        let (_root, store) = store();
        let outbox = Outbox::open(&store).unwrap();
        for ts in 1..=i64::try_from(CAP).unwrap() + 2 {
            outbox.push(crash(ts)).unwrap();
        }
        let seqs = held(&outbox);
        assert_eq!(seqs.len(), CAP);
        assert_eq!(seqs.first(), Some(&3));
        assert_eq!(outbox.dropped(), 2);
        drop(outbox);
        assert_eq!(Outbox::open(&store).unwrap().dropped(), 2);
    }

    #[test]
    fn preparing_the_store_keeps_the_outbox() {
        let (_root, store) = store();
        Outbox::open(&store).unwrap().push(crash(7)).unwrap();
        store.prepare().unwrap();
        assert_eq!(held(&Outbox::open(&store).unwrap()), [7]);
    }

    #[test]
    fn backoff_doubles_to_its_cap_honors_retry_after_and_resets() {
        let mut backoff = Backoff::default();
        let waits: Vec<Duration> = (0..12).map(|_| backoff.next(None)).collect();
        for (n, wait) in waits.iter().enumerate() {
            let ceiling = (BACKOFF_FLOOR * 2_u32.pow(u32::try_from(n).unwrap())).min(BACKOFF_CAP);
            assert!(
                *wait >= ceiling / 2 && *wait <= ceiling,
                "wait {n} was {wait:?}"
            );
        }
        assert!(waits.last().is_some_and(|wait| *wait >= BACKOFF_CAP / 2));
        assert_eq!(
            backoff.next(Some(Duration::from_secs(7))),
            Duration::from_secs(7)
        );
        assert_eq!(backoff.next(Some(Duration::from_secs(3600))), BACKOFF_CAP);
        backoff.reset();
        assert!(backoff.next(None) <= BACKOFF_FLOOR);
    }

    /// A scripted answer: the status and its Retry-After header, if any.
    type Reply = (StatusCode, Option<&'static str>);

    /// A Cloud that answers each push with the next scripted reply and keeps
    /// every body it was sent.
    #[derive(Clone, Default)]
    struct Cloud {
        replies: Arc<Mutex<Vec<Reply>>>,
        bodies: Arc<Mutex<Vec<serde_json::Value>>>,
    }

    impl Cloud {
        async fn serve(replies: Vec<Reply>) -> (Self, Url) {
            let cloud = Self {
                replies: Arc::new(Mutex::new(replies.into_iter().rev().collect())),
                bodies: Arc::default(),
            };
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let app = Router::new()
                .route("/push", post(Self::push))
                .with_state(cloud.clone());
            tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            (cloud, format!("http://{address}/push").parse().unwrap())
        }

        async fn push(
            State(cloud): State<Self>,
            Json(body): Json<serde_json::Value>,
        ) -> axum::response::Response {
            cloud.bodies.lock().unwrap().push(body);
            let (status, retry_after) = cloud.replies.lock().unwrap().pop().unwrap();
            let mut response = axum::response::IntoResponse::into_response(status);
            if let Some(seconds) = retry_after {
                response
                    .headers_mut()
                    .insert(RETRY_AFTER, seconds.parse().unwrap());
            }
            response
        }

        fn bodies(&self) -> Vec<serde_json::Value> {
            self.bodies.lock().unwrap().clone()
        }
    }

    #[tokio::test]
    async fn entries_leave_the_outbox_only_when_cloud_acks_them() {
        let (_root, store) = store();
        let outbox = Outbox::open(&store).unwrap();
        outbox.push(crash(5)).unwrap();
        outbox.push(crash(9)).unwrap();
        let (cloud, url) = Cloud::serve(vec![
            (StatusCode::SERVICE_UNAVAILABLE, None),
            (StatusCode::UNAUTHORIZED, None),
            (StatusCode::NOT_FOUND, None),
            (StatusCode::NO_CONTENT, None),
        ])
        .await;
        let shipper = Shipper::new(outbox.clone(), move || Some(url.clone())).unwrap();

        for _ in 0..3 {
            assert_eq!(shipper.ship_once().await, Shipped::Retry { after: None });
            assert_eq!(held(&outbox), [5, 9]);
        }
        assert_eq!(shipper.ship_once().await, Shipped::Progress);
        assert_eq!(held(&outbox), [] as [u64; 0]);
        assert_eq!(shipper.ship_once().await, Shipped::Idle);

        let bodies = cloud.bodies();
        assert_eq!(bodies.len(), 4);
        assert_eq!(
            bodies.last(),
            Some(&serde_json::json!({
                "stream": "crash",
                "seq": "9",
                "dropped": 0,
                "events": [
                    { "seq": "5", "marker": serde_json::to_value(crash(5)).unwrap() },
                    { "seq": "9", "marker": serde_json::to_value(crash(9)).unwrap() },
                ],
            }))
        );
    }

    #[tokio::test]
    async fn a_batch_cloud_refuses_is_dropped_and_counted() {
        let (_root, store) = store();
        let outbox = Outbox::open(&store).unwrap();
        outbox.push(crash(5)).unwrap();
        let (cloud, url) = Cloud::serve(vec![
            (StatusCode::UNPROCESSABLE_ENTITY, None),
            (StatusCode::NO_CONTENT, None),
        ])
        .await;
        let shipper = Shipper::new(outbox.clone(), move || Some(url.clone())).unwrap();

        assert_eq!(shipper.ship_once().await, Shipped::Progress);
        assert_eq!(held(&outbox), [] as [u64; 0]);
        outbox.push(crash(6)).unwrap();
        assert_eq!(shipper.ship_once().await, Shipped::Progress);
        let dropped: Vec<_> = cloud
            .bodies()
            .iter()
            .map(|body| body["dropped"].clone())
            .collect();
        assert_eq!(dropped, [0, 1]);
    }

    #[tokio::test]
    async fn rate_limits_and_outages_keep_the_batch_and_pass_on_retry_after() {
        let (_root, store) = store();
        let outbox = Outbox::open(&store).unwrap();
        outbox.push(crash(5)).unwrap();
        let (_cloud, url) = Cloud::serve(vec![
            (StatusCode::TOO_MANY_REQUESTS, Some("12")),
            (StatusCode::TOO_MANY_REQUESTS, None),
            (StatusCode::BAD_GATEWAY, Some("3")),
        ])
        .await;
        let shipper = Shipper::new(outbox.clone(), move || Some(url.clone())).unwrap();

        assert_eq!(
            shipper.ship_once().await,
            Shipped::Retry {
                after: Some(Duration::from_secs(12))
            }
        );
        assert_eq!(shipper.ship_once().await, Shipped::Retry { after: None });
        assert_eq!(
            shipper.ship_once().await,
            Shipped::Retry {
                after: Some(Duration::from_secs(3))
            }
        );
        assert_eq!(held(&outbox), [5]);
    }

    #[tokio::test]
    async fn a_network_error_keeps_the_batch() {
        let (_root, store) = store();
        let outbox = Outbox::open(&store).unwrap();
        outbox.push(crash(5)).unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url: Url = format!("http://{}/push", listener.local_addr().unwrap())
            .parse()
            .unwrap();
        drop(listener);
        let shipper = Shipper::new(outbox.clone(), move || Some(url.clone())).unwrap();
        assert_eq!(shipper.ship_once().await, Shipped::Retry { after: None });
        assert_eq!(held(&outbox), [5]);
    }

    #[tokio::test]
    async fn without_a_target_the_outbox_only_buffers() {
        let (_root, store) = store();
        let outbox = Outbox::open(&store).unwrap();
        outbox.push(crash(5)).unwrap();
        let shipper = Shipper::new(outbox.clone(), push_target).unwrap();
        assert_eq!(shipper.ship_once().await, Shipped::Idle);
        assert_eq!(held(&outbox), [5]);
    }

    #[test]
    fn a_crash_batch_serializes_seqs_as_decimal_strings() {
        const SEQ: u64 = 1_760_000_000_123_456_789;
        let events = [Entry {
            seq: SEQ,
            marker: crash(i64::try_from(SEQ).unwrap()),
        }];
        let batch = Batch {
            stream: STREAM,
            seq: SEQ,
            dropped: 3,
            events: &events,
        };
        let json = serde_json::to_string(&batch).unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&json).unwrap(),
            serde_json::json!({
                "stream": "crash",
                "seq": "1760000000123456789",
                "dropped": 3,
                "events": [{
                    "seq": "1760000000123456789",
                    "marker": {
                        "machine_id": "a".repeat(32),
                        "container_id": "b".repeat(64),
                        "service_id": "c".repeat(32),
                        "service": "prod/web",
                        "deployment_id": "d1",
                        "replica": "web-1",
                        "ts": 1_760_000_000_123_456_789_i64,
                        "kind": "exit",
                        "exit_code": 137,
                        "oom": true,
                        "stopped": false,
                    },
                }],
            })
        );
    }
}
