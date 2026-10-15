//! Ships outbox rows to Cloud, one signed batch at a time per stream, and
//! keeps the salt cache current from Cloud's acks.

mod pageviews;

use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, PoisonError},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
pub(super) use pageviews::PageviewOutbox;
#[cfg(test)]
pub(super) use pageviews::dir_has;
use ployz_core::{MachineId, metrics::MetricMarker};
use reqwest::{
    RequestBuilder, StatusCode, Url,
    header::{CONTENT_TYPE, RETRY_AFTER},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::Notify;

use super::{
    layout::{StoreRoot, create_store_dir},
    salts::Salts,
};
use crate::{machine::LocalMachineRecord, management::ManagementSecret};

pub(super) const CAP: usize = 1000;
const BATCH: usize = 100;
const LAST_SEQ_FILE: &str = "last_seq";
const DROPPED_FILE: &str = "dropped";
const CRASH: &str = "crash";
const PAGEVIEWS: &str = "pageviews";
const TICK: Duration = Duration::from_secs(30);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const BACKOFF_FLOOR: Duration = Duration::from_secs(1);
const BACKOFF_CAP: Duration = Duration::from_secs(5 * 60);
const RETRY_AFTER_CAP: Duration = Duration::from_secs(60 * 60);
const UNHEARD_WARN_EVERY: Duration = Duration::from_secs(5 * 60);
/// How often a stale salt cache may ask Cloud again. An older Cloud answers
/// without salts, so the cache stays stale and would otherwise ask every tick.
const SALT_REFRESH_EVERY: Duration = Duration::from_secs(5 * 60);

const MACHINE_FILE: &str = "machine.json";
const SIGNATURE_CONTEXT: &[u8] = b"ployz-push-v1\0";
const TIMESTAMP_HEADER: &str = "Ployz-Timestamp";
const SIGNATURE_HEADER: &str = "Ployz-Signature";

/// Where batches go and who signs them. The secret stays in memory for one
/// ship cycle and is never logged.
pub struct Pusher {
    url: Url,
    machine_id: MachineId,
    secret: ManagementSecret,
}

/// Read from machine.json before every send, so Cloud setting or clearing
/// the target and a Machine reset take effect without a restart. None until
/// Cloud sets a target, so the outbox only buffers.
pub fn push_target(data_dir: &Path) -> Option<Pusher> {
    let bytes = fs::read(data_dir.join(MACHINE_FILE)).ok()?;
    let record = serde_json::from_slice::<LocalMachineRecord>(&bytes).ok()?;
    Some(Pusher {
        url: record.push_target()?.clone(),
        machine_id: record.id(),
        secret: record.management_secret().clone(),
    })
}

/// Signs the exact body bytes with the Management Identity's key; Cloud
/// verifies against the identity it stored at enrollment.
fn authenticate(
    request: RequestBuilder,
    pusher: &Pusher,
    ts: i64,
    body: Vec<u8>,
) -> RequestBuilder {
    let signature = signature(&pusher.secret, &pusher.machine_id, ts, &body);
    request
        .header(CONTENT_TYPE, "application/json")
        .header(TIMESTAMP_HEADER, ts.to_string())
        .header(SIGNATURE_HEADER, format!("v1={signature}"))
        .body(body)
}

fn signature(secret: &ManagementSecret, machine_id: &MachineId, ts: i64, body: &[u8]) -> String {
    let mut message = SIGNATURE_CONTEXT.to_vec();
    message.extend_from_slice(machine_id.as_str().as_bytes());
    message.push(0);
    message.extend_from_slice(&ts.to_be_bytes());
    message.extend_from_slice(&Sha256::digest(body));
    URL_SAFE_NO_PAD.encode(secret.sign(&message))
}

pub(super) fn unix_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_secs()).unwrap_or(i64::MAX)
        })
}

/// A seq after `last`: the clock in nanos, or `last + 1` when the clock
/// has not moved past it.
fn next_seq(clock: u64, last: u64) -> u64 {
    clock.max(last.saturating_add(1))
}

/// One batch read from an outbox and waiting on Cloud's answer.
#[derive(Debug)]
pub(super) struct Held {
    body: Vec<u8>,
    /// The entries or segments that settle together when Cloud answers.
    seqs: Vec<u64>,
}

/// An outbox the shipper drains, one stream of the push wire.
pub(super) trait Stream: Send + Sync + 'static {
    const NAME: &'static str;

    /// The oldest batch waiting for Cloud, or None when nothing waits.
    fn next(&self) -> io::Result<Option<Held>>;

    /// Deletes what Cloud took.
    fn ack(&self, seqs: &[u64]) -> io::Result<()>;

    /// Deletes what Cloud refused and counts it as dropped.
    fn refuse(&self, seqs: &[u64]) -> io::Result<()>;

    fn wake(&self) -> &Notify;

    /// An empty batch whose only purpose is the salts in its ack, for the
    /// stream Cloud already answers.
    fn salt_refresh(&self) -> io::Result<Option<Held>> {
        Ok(None)
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub(super) struct Entry {
    #[serde(with = "decimal")]
    pub(super) seq: u64,
    pub(super) marker: MetricMarker,
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

#[derive(Debug)]
struct Ledger {
    last_seq: u64,
    dropped: u64,
}

impl Ledger {
    fn open(dir: &Path, newest: u64) -> Self {
        Self {
            last_seq: read_counter(&dir.join(LAST_SEQ_FILE)).max(newest),
            dropped: read_counter(&dir.join(DROPPED_FILE)),
        }
    }

    fn take_seq(&mut self, dir: &Path, clock: u64) -> io::Result<u64> {
        let seq = next_seq(clock, self.last_seq);
        write_counter(&dir.join(LAST_SEQ_FILE), seq)?;
        self.last_seq = seq;
        Ok(seq)
    }

    fn count_dropped(&mut self, dir: &Path, count: usize) -> io::Result<()> {
        self.dropped = self
            .dropped
            .saturating_add(u64::try_from(count).unwrap_or(u64::MAX));
        write_counter(&dir.join(DROPPED_FILE), self.dropped)
    }
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

    pub fn push(&self, marker: MetricMarker) -> io::Result<()> {
        let mut ledger = self.ledger.lock().unwrap_or_else(PoisonError::into_inner);
        let seqs = seqs(&self.dir, "")?;
        let excess = (seqs.len() + 1).saturating_sub(CAP);
        if excess > 0 {
            for seq in seqs.iter().take(excess) {
                remove_entry(&self.dir, *seq, "")?;
            }
            ledger.count_dropped(&self.dir, excess)?;
            tracing::warn!(
                dropped = excess,
                "the crash outbox is full; dropping its oldest entries"
            );
        }
        let seq = ledger.take_seq(&self.dir, u64::try_from(marker.ts).unwrap_or(0))?;
        let entry = Entry { seq, marker };
        let json = serde_json::to_vec(&entry).map_err(io::Error::other)?;
        crate::filesystem::atomic_write(&self.dir.join(entry_name(seq, "")), &json, 0o600)?;
        drop(ledger);
        self.wake.notify_one();
        Ok(())
    }

    pub(super) fn oldest(&self, limit: usize) -> io::Result<Vec<Entry>> {
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

    fn remove(&self, seqs: &[u64], refused: bool) -> io::Result<()> {
        let mut ledger = self.ledger.lock().unwrap_or_else(PoisonError::into_inner);
        for seq in seqs {
            remove_entry(&self.dir, *seq, "")?;
        }
        if refused {
            ledger.count_dropped(&self.dir, seqs.len())?;
        }
        Ok(())
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
        self.remove(seqs, false)
    }

    fn refuse(&self, seqs: &[u64]) -> io::Result<()> {
        self.remove(seqs, true)
    }

    fn wake(&self) -> &Notify {
        &self.wake
    }

    /// Carries the crash cursor Cloud already holds, so its `greatest()`
    /// cursor makes the batch harmless.
    fn salt_refresh(&self) -> io::Result<Option<Held>> {
        let (last_seq, _) = self.ledger();
        self.batch(last_seq, &[]).map(Some)
    }
}

const ENTRY_NAME_LEN: usize = 20;

fn entry_name(seq: u64, suffix: &str) -> String {
    format!("{seq:0ENTRY_NAME_LEN$}{suffix}")
}

/// The seqs of the entries in `dir` named `<20 digits><suffix>`, oldest
/// first.
fn seqs(dir: &Path, suffix: &str) -> io::Result<Vec<u64>> {
    let mut seqs = Vec::new();
    for entry in fs::read_dir(dir)? {
        let name = entry?.file_name();
        let Some(name) = name.to_str().and_then(|name| name.strip_suffix(suffix)) else {
            continue;
        };
        if name.len() == ENTRY_NAME_LEN
            && let Ok(seq) = name.parse()
        {
            seqs.push(seq);
        }
    }
    seqs.sort_unstable();
    Ok(seqs)
}

fn remove_entry(dir: &Path, seq: u64, suffix: &str) -> io::Result<()> {
    match fs::remove_file(dir.join(entry_name(seq, suffix))) {
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

#[derive(Debug, PartialEq)]
enum Shipped {
    Idle,
    Progress,
    Retry { after: Option<Duration> },
}

pub struct Shipper<S, T> {
    outbox: Arc<S>,
    salts: Arc<Salts>,
    client: reqwest::Client,
    target: T,
    /// When the journal last heard that Cloud does not know this Machine.
    unheard_warned: Mutex<Option<Instant>>,
    /// When a stale salt cache last asked Cloud for salts.
    salts_asked: Mutex<Option<Instant>>,
}

impl<S: Stream, T: Fn() -> Option<Pusher>> Shipper<S, T> {
    pub fn new(outbox: Arc<S>, salts: Arc<Salts>, target: T) -> reqwest::Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()?;
        Ok(Self {
            outbox,
            salts,
            client,
            target,
            unheard_warned: Mutex::new(None),
            salts_asked: Mutex::new(None),
        })
    }

    pub async fn run(self) {
        let mut backoff = Backoff::default();
        loop {
            match self.ship_once().await {
                Shipped::Progress => backoff.reset(),
                Shipped::Idle => {
                    backoff.reset();
                    tokio::select! {
                        () = self.outbox.wake().notified() => {}
                        () = tokio::time::sleep(TICK) => {}
                    }
                }
                Shipped::Retry { after } => tokio::time::sleep(backoff.next(after)).await,
            }
        }
    }

    async fn ship_once(&self) -> Shipped {
        let Some(pusher) = (self.target)() else {
            return Shipped::Idle;
        };
        let held = match self.outbox.next() {
            Ok(Some(held)) => held,
            Ok(None) if self.asks_for_salts() => match self.outbox.salt_refresh() {
                Ok(Some(held)) => held,
                Ok(None) => return Shipped::Idle,
                Err(error) => {
                    tracing::warn!(stream = S::NAME, %error, "cannot build a salt refresh");
                    return Shipped::Retry { after: None };
                }
            },
            Ok(None) => return Shipped::Idle,
            Err(error) => {
                tracing::warn!(stream = S::NAME, %error, "cannot read the outbox");
                return Shipped::Retry { after: None };
            }
        };
        let request = self.client.post(pusher.url.clone());
        let response = match authenticate(request, &pusher, unix_seconds(), held.body)
            .send()
            .await
        {
            Ok(response) => response,
            Err(error) => {
                tracing::debug!(stream = S::NAME, %error, "cannot reach Cloud to ship a batch");
                return Shipped::Retry { after: None };
            }
        };
        let status = response.status();
        let settled = match status {
            StatusCode::OK => {
                self.accept_salts(response).await;
                self.outbox.ack(&held.seqs)
            }
            status if status.is_success() => self.outbox.ack(&held.seqs),
            StatusCode::BAD_REQUEST
            | StatusCode::PAYLOAD_TOO_LARGE
            | StatusCode::UNPROCESSABLE_ENTITY => {
                tracing::warn!(stream = S::NAME, %status, "Cloud refused a batch; dropping it");
                self.outbox.refuse(&held.seqs)
            }
            status if status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error() => {
                return Shipped::Retry {
                    after: retry_after(&response),
                };
            }
            // Cloud answers 404 for pageviews until it ingests them, so the
            // outbox holds them up to its cap.
            StatusCode::NOT_FOUND if S::NAME == PAGEVIEWS => {
                tracing::debug!("Cloud does not take pageviews yet; holding them");
                return Shipped::Retry { after: None };
            }
            StatusCode::UNAUTHORIZED | StatusCode::NOT_FOUND => {
                if self.warn_unheard() {
                    tracing::warn!(stream = S::NAME, %status, "Cloud does not accept this Machine's batches; keeping them");
                }
                return Shipped::Retry { after: None };
            }
            status => {
                tracing::debug!(stream = S::NAME, %status, "Cloud did not take a batch; keeping it");
                return Shipped::Retry { after: None };
            }
        };
        match settled {
            Ok(()) => Shipped::Progress,
            Err(error) => {
                tracing::warn!(stream = S::NAME, %error, "cannot delete shipped outbox entries");
                Shipped::Retry { after: None }
            }
        }
    }

    async fn accept_salts(&self, response: reqwest::Response) {
        let body = match response.bytes().await {
            Ok(body) => body,
            Err(error) => {
                tracing::debug!(%error, "cannot read Cloud's ack");
                return;
            }
        };
        if let Err(error) = self.salts.accept(&body, unix_seconds()) {
            tracing::warn!(%error, "cannot keep the salts Cloud sent");
        }
    }
}

impl<S, T> Shipper<S, T> {
    /// Whether to warn that Cloud turned this Machine away. A Machine Cloud
    /// lost keeps retrying, so the journal hears it once every few minutes.
    fn warn_unheard(&self) -> bool {
        once_every(&self.unheard_warned, UNHEARD_WARN_EVERY)
    }

    fn asks_for_salts(&self) -> bool {
        self.salts.stale(unix_seconds()) && once_every(&self.salts_asked, SALT_REFRESH_EVERY)
    }
}

/// Whether `every` has passed since `last`, marking now when it has.
fn once_every(last: &Mutex<Option<Instant>>, every: Duration) -> bool {
    let now = Instant::now();
    let mut last = last.lock().unwrap_or_else(PoisonError::into_inner);
    if last.is_some_and(|at| now.duration_since(at) < every) {
        return false;
    }
    *last = Some(now);
    true
}

fn retry_after(response: &reqwest::Response) -> Option<Duration> {
    let seconds = response.headers().get(RETRY_AFTER)?.to_str().ok()?;
    seconds.trim().parse().ok().map(Duration::from_secs)
}

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
    fn next(&mut self, asked: Option<Duration>) -> Duration {
        let ceiling = self.ceiling;
        self.ceiling = (ceiling * 2).min(BACKOFF_CAP);
        if let Some(asked) = asked {
            return asked.min(RETRY_AFTER_CAP);
        }
        let half = ceiling / 2;
        half + half.mul_f64(fastrand::f64())
    }

    fn reset(&mut self) {
        self.ceiling = BACKOFF_FLOOR;
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
mod tests {
    use std::sync::{Arc, Mutex};

    use axum::{
        Router,
        body::Bytes,
        extract::State,
        http::{HeaderMap, StatusCode},
        response::IntoResponse,
        routing::post,
    };
    use ployz_core::{ContainerId, MachineId, QualifiedService, ServiceId, metrics::MarkerKind};

    use super::*;
    use crate::{machine::LocalMachineStore, test_dir::TestDir};

    const KAT_MACHINE: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn seeded(byte: u8) -> ManagementSecret {
        serde_json::from_value(serde_json::to_value([byte; 32]).unwrap()).unwrap()
    }

    fn pusher(url: &Url) -> Pusher {
        Pusher {
            url: url.clone(),
            machine_id: MachineId::parse(KAT_MACHINE).unwrap(),
            secret: seeded(7),
        }
    }

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

    fn fresh_salts(store: &StoreRoot) -> Arc<Salts> {
        let salts = Salts::open(store, 0).unwrap();
        salts.accept(br#"{"sites":[]}"#, unix_seconds()).unwrap();
        Arc::new(salts)
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
        Stream::ack(&*reopened, &held(&reopened)).unwrap();
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
        assert_eq!(outbox.ledger().1, 2);
        drop(outbox);
        assert_eq!(Outbox::open(&store).unwrap().ledger().1, 2);
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
        assert_eq!(
            backoff.next(Some(Duration::from_secs(3600))),
            Duration::from_secs(3600)
        );
        assert_eq!(
            backoff.next(Some(Duration::from_secs(86_400))),
            RETRY_AFTER_CAP
        );
        backoff.reset();
        assert!(backoff.next(None) <= BACKOFF_FLOOR);
    }

    /// A status and its Retry-After, or for a 200 the ack body.
    type Reply = (StatusCode, Option<&'static str>);

    #[derive(Clone, Default)]
    struct Cloud {
        replies: Arc<Mutex<Vec<Reply>>>,
        bodies: Arc<Mutex<Vec<serde_json::Value>>>,
        requests: Arc<Mutex<Vec<(HeaderMap, Bytes)>>>,
    }

    impl Cloud {
        async fn serve(replies: Vec<Reply>) -> (Self, Url) {
            let cloud = Self {
                replies: Arc::new(Mutex::new(replies.into_iter().rev().collect())),
                bodies: Arc::default(),
                requests: Arc::default(),
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
            headers: HeaderMap,
            body: Bytes,
        ) -> axum::response::Response {
            cloud
                .bodies
                .lock()
                .unwrap()
                .push(serde_json::from_slice(&body).unwrap());
            cloud.requests.lock().unwrap().push((headers, body));
            let (status, extra) = cloud.replies.lock().unwrap().pop().unwrap();
            if status == StatusCode::OK {
                let json = [(CONTENT_TYPE, "application/json")];
                return (status, json, extra.unwrap_or_default()).into_response();
            }
            let mut response = status.into_response();
            if let Some(seconds) = extra {
                response
                    .headers_mut()
                    .insert(RETRY_AFTER, seconds.parse().unwrap());
            }
            response
        }

        fn bodies(&self) -> Vec<serde_json::Value> {
            self.bodies.lock().unwrap().clone()
        }

        fn requests(&self) -> Vec<(HeaderMap, Bytes)> {
            self.requests.lock().unwrap().clone()
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
        let shipper = Shipper::new(outbox.clone(), fresh_salts(&store), move || {
            Some(pusher(&url))
        })
        .unwrap();

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
    async fn cloud_turning_the_machine_away_warns_once_every_few_minutes() {
        let (_root, store) = store();
        let outbox = Outbox::open(&store).unwrap();
        outbox.push(crash(5)).unwrap();
        let (_cloud, url) = Cloud::serve(vec![
            (StatusCode::UNAUTHORIZED, None),
            (StatusCode::NOT_FOUND, None),
            (StatusCode::UNAUTHORIZED, None),
        ])
        .await;
        let shipper = Shipper::new(outbox.clone(), fresh_salts(&store), move || {
            Some(pusher(&url))
        })
        .unwrap();
        let (journal, _journal) = crate::diag::test_support::capture();

        shipper.ship_once().await;
        shipper.ship_once().await;
        let warned = shipper.unheard_warned.lock().unwrap().unwrap();
        *shipper.unheard_warned.lock().unwrap() = warned.checked_sub(UNHEARD_WARN_EVERY);
        shipper.ship_once().await;

        let journal = journal.text();
        let warnings: Vec<&str> = journal
            .lines()
            .filter(|line| line.contains("WARN"))
            .collect();
        assert_eq!(warnings.len(), 2, "{journal}");
        assert!(
            warnings.iter().all(|line| line.contains("status=401")),
            "{journal}"
        );
        assert_eq!(held(&outbox), [5]);
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
        let shipper = Shipper::new(outbox.clone(), fresh_salts(&store), move || {
            Some(pusher(&url))
        })
        .unwrap();

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
        let shipper = Shipper::new(outbox.clone(), fresh_salts(&store), move || {
            Some(pusher(&url))
        })
        .unwrap();

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
        let shipper = Shipper::new(outbox.clone(), fresh_salts(&store), move || {
            Some(pusher(&url))
        })
        .unwrap();
        assert_eq!(shipper.ship_once().await, Shipped::Retry { after: None });
        assert_eq!(held(&outbox), [5]);
    }

    #[tokio::test]
    async fn without_a_target_the_outbox_only_buffers() {
        let (_root, store) = store();
        let outbox = Outbox::open(&store).unwrap();
        outbox.push(crash(5)).unwrap();
        let data = tempfile::tempdir().unwrap();
        let data_dir = data.path().to_path_buf();
        let shipper = Shipper::new(outbox.clone(), fresh_salts(&store), move || {
            push_target(&data_dir)
        })
        .unwrap();
        assert_eq!(shipper.ship_once().await, Shipped::Idle);
        drop(LocalMachineStore::open(data.path()).unwrap());
        assert_eq!(shipper.ship_once().await, Shipped::Idle);
        assert_eq!(held(&outbox), [5]);
    }

    #[test]
    fn the_target_follows_machine_json_on_every_read() {
        let data = tempfile::tempdir().unwrap();
        let url: Url = "https://cloud.example/api/machines/m/observe"
            .parse()
            .unwrap();
        let mut machine = LocalMachineStore::open(data.path()).unwrap();
        assert!(push_target(data.path()).is_none());

        machine.set_push_target(Some(url.clone())).unwrap();
        let pusher = push_target(data.path()).unwrap();
        assert_eq!(pusher.url, url);
        assert_eq!(pusher.machine_id, machine.record().id());
        assert_eq!(
            pusher.secret.public_key(),
            machine.record().management_secret().public_key()
        );

        machine.set_push_target(None).unwrap();
        assert!(push_target(data.path()).is_none());
    }

    #[test]
    fn the_signature_matches_the_shared_known_answer_vector() {
        let secret = seeded(7);
        assert_eq!(
            hex::encode(secret.public_key().as_bytes()),
            "ea4a6c63e29c520abef5507b132ec5f9954776aebebe7b92421eea691446d22c"
        );
        let body = br#"{"stream":"crash","seq":"1760000000123456789","dropped":0,"events":[]}"#;
        assert_eq!(
            signature(
                &secret,
                &MachineId::parse(KAT_MACHINE).unwrap(),
                1_760_000_000,
                body
            ),
            "rOUkHmKtJRTf53MNT4z0t9gV3yOkQnJ_yriYa5G7ebHo5-VrHUa0spxdC56PowBMo9JEMEy_JXT97yIG11RPAw"
        );
    }

    #[tokio::test]
    async fn a_push_is_signed_over_the_exact_bytes_cloud_receives() {
        let (_root, store) = store();
        let outbox = Outbox::open(&store).unwrap();
        outbox.push(crash(5)).unwrap();
        let (cloud, url) = Cloud::serve(vec![(StatusCode::NO_CONTENT, None)]).await;
        let shipper =
            Shipper::new(outbox, fresh_salts(&store), move || Some(pusher(&url))).unwrap();
        let before = unix_seconds();
        assert_eq!(shipper.ship_once().await, Shipped::Progress);

        let [(headers, body)] = <[_; 1]>::try_from(cloud.requests()).unwrap();
        let header = |name: &str| headers.get(name).unwrap().to_str().unwrap().to_owned();
        assert_eq!(header("content-type"), "application/json");
        let ts: i64 = header("ployz-timestamp").parse().unwrap();
        assert!((before..=unix_seconds()).contains(&ts));
        let signature = header("ployz-signature");
        let signature = URL_SAFE_NO_PAD
            .decode(signature.strip_prefix("v1=").unwrap())
            .unwrap();

        let mut message = b"ployz-push-v1\0".to_vec();
        message.extend_from_slice(KAT_MACHINE.as_bytes());
        message.push(0);
        message.extend_from_slice(&ts.to_be_bytes());
        message.extend_from_slice(&Sha256::digest(&body));
        let public = iroh::PublicKey::from_bytes(seeded(7).public_key().as_bytes()).unwrap();
        let signature = iroh::Signature::from_bytes(&signature.try_into().unwrap());
        public.verify(&message, &signature).unwrap();

        message.pop();
        message.push(b'!');
        assert!(public.verify(&message, &signature).is_err());
    }

    #[test]
    fn a_crash_batch_serializes_seqs_as_decimal_strings() {
        const SEQ: u64 = 1_760_000_000_123_456_789;
        let events = [Entry {
            seq: SEQ,
            marker: crash(i64::try_from(SEQ).unwrap()),
        }];
        let batch = Batch {
            stream: CRASH,
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

    const ACK: &str = r#"{"sites":[{"namespace":"acme-prod","salts":[{"day":"2026-10-15","start":0,"end":9999999999,"salt":"AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8"}]}]}"#;

    fn acme_covered(salts: &Salts) -> bool {
        let acme = ployz_core::Namespace::parse("acme-prod").unwrap();
        salts.lookup(&acme, unix_seconds(), |lookup| {
            matches!(lookup, super::super::salts::Lookup::Covered(_))
        })
    }

    #[tokio::test]
    async fn a_200_ack_replaces_the_salts_and_a_204_or_an_odd_body_keeps_them() {
        let (_root, store) = store();
        let outbox = Outbox::open(&store).unwrap();
        let (_cloud, url) = Cloud::serve(vec![
            (StatusCode::OK, Some(ACK)),
            (StatusCode::NO_CONTENT, None),
            (StatusCode::OK, Some("not json")),
        ])
        .await;
        let salts = Arc::new(Salts::open(&store, 0).unwrap());
        let shipper =
            Shipper::new(outbox.clone(), salts.clone(), move || Some(pusher(&url))).unwrap();

        assert_eq!(shipper.ship_once().await, Shipped::Progress);
        assert!(acme_covered(&salts));
        for ts in [5, 6] {
            outbox.push(crash(ts)).unwrap();
            assert_eq!(shipper.ship_once().await, Shipped::Progress);
            assert!(acme_covered(&salts));
        }
        assert!(!salts.stale(unix_seconds()));
        assert_eq!(held(&outbox), [] as [u64; 0]);
    }

    #[tokio::test]
    #[expect(clippy::indexing_slicing, reason = "JSON fixture assertions")]
    async fn a_stale_cache_asks_with_an_empty_crash_batch_at_the_crash_cursor() {
        let (_root, store) = store();
        let outbox = Outbox::open(&store).unwrap();
        let (cloud, url) = Cloud::serve(vec![
            (StatusCode::NO_CONTENT, None),
            (StatusCode::NO_CONTENT, None),
            (StatusCode::OK, Some(ACK)),
        ])
        .await;
        let salts = Arc::new(Salts::open(&store, 0).unwrap());
        let shipper =
            Shipper::new(outbox.clone(), salts.clone(), move || Some(pusher(&url))).unwrap();

        assert_eq!(shipper.ship_once().await, Shipped::Progress);
        assert_eq!(shipper.ship_once().await, Shipped::Idle);
        *shipper.salts_asked.lock().unwrap() = None;
        outbox.push(crash(5)).unwrap();
        assert_eq!(shipper.ship_once().await, Shipped::Progress);
        assert_eq!(shipper.ship_once().await, Shipped::Progress);
        assert!(!salts.stale(unix_seconds()));
        assert_eq!(shipper.ship_once().await, Shipped::Idle);

        let bodies = cloud.bodies();
        let empty = |seq: &str| serde_json::json!({ "stream": "crash", "seq": seq, "dropped": 0, "events": [] });
        assert_eq!(bodies.len(), 3);
        assert_eq!(bodies[0], empty("0"));
        assert_eq!(bodies[1]["seq"], "5");
        assert_eq!(bodies[2], empty("5"));
    }

    fn pageviews(store: &StoreRoot, rows: usize) -> Arc<PageviewOutbox> {
        let outbox = PageviewOutbox::open(store).unwrap();
        for n in 0..rows {
            outbox
                .push(&serde_json::json!({ "kind": "pageview", "n": n }))
                .unwrap();
        }
        outbox
    }

    #[tokio::test]
    #[expect(clippy::indexing_slicing, reason = "JSON fixture assertions")]
    async fn pageviews_cloud_does_not_take_yet_stay_held_without_a_warning() {
        let (_root, store) = store();
        let outbox = pageviews(&store, 200);
        let (cloud, url) = Cloud::serve(vec![
            (StatusCode::NOT_FOUND, None),
            (StatusCode::NOT_FOUND, None),
            (StatusCode::NO_CONTENT, None),
        ])
        .await;
        let shipper = Shipper::new(outbox.clone(), fresh_salts(&store), move || {
            Some(pusher(&url))
        })
        .unwrap();
        let (journal, _journal) = crate::diag::test_support::capture();

        for _ in 0..2 {
            assert_eq!(shipper.ship_once().await, Shipped::Retry { after: None });
            assert_eq!(outbox.held_rows(), 200);
        }
        assert_eq!(shipper.ship_once().await, Shipped::Progress);
        assert_eq!(outbox.held_rows(), 0);
        assert!(!journal.text().contains("WARN"), "{}", journal.text());

        let bodies = cloud.bodies();
        let last = bodies.last().unwrap();
        assert_eq!(last["stream"], "pageviews");
        assert_eq!(last["dropped"], 0);
        assert_eq!(last["events"].as_array().unwrap().len(), 200);
        assert_eq!(
            last["events"][199],
            serde_json::json!({ "kind": "pageview", "n": 199 })
        );
    }

    #[tokio::test]
    async fn the_pageviews_shipper_never_asks_for_salts() {
        let (_root, store) = store();
        let outbox = pageviews(&store, 0);
        let (cloud, url) = Cloud::serve(Vec::new()).await;
        let salts = Arc::new(Salts::open(&store, 0).unwrap());
        let shipper = Shipper::new(outbox, salts, move || Some(pusher(&url))).unwrap();
        assert_eq!(shipper.ship_once().await, Shipped::Idle);
        assert!(cloud.bodies().is_empty());
    }
}
