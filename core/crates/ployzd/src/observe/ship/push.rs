//! Pushing a stream to Cloud: the signed request, the retry rhythm, and what each status
//! Cloud answers means for the batch.

use std::{
    io,
    path::Path,
    sync::{Arc, Mutex, PoisonError},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use ployz_core::MachineId;
use reqwest::{
    RequestBuilder, StatusCode, Url,
    header::{CONTENT_TYPE, RETRY_AFTER},
};
use sha2::{Digest, Sha256};
use tokio::sync::Notify;

use crate::{machine::store, management::ManagementSecret, observe::salts::Salts};

const TICK: Duration = Duration::from_secs(30);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const BACKOFF_FLOOR: Duration = Duration::from_secs(1);
const BACKOFF_CAP: Duration = Duration::from_secs(5 * 60);
const RETRY_AFTER_CAP: Duration = Duration::from_secs(60 * 60);
const UNHEARD_WARN_EVERY: Duration = Duration::from_secs(5 * 60);
const SALT_REFRESH_EVERY: Duration = Duration::from_secs(5 * 60);

const SIGNATURE_CONTEXT: &[u8] = b"ployz-push-v1\0";
const TIMESTAMP_HEADER: &str = "Ployz-Timestamp";
const SIGNATURE_HEADER: &str = "Ployz-Signature";

/// What Cloud takes in one push of any stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct PushLimits {
    pub(super) rows: usize,
    pub(super) bytes: usize,
}

impl PushLimits {
    /// Cloud answers 413 to a body past 256 KiB and 400 to more than 1000 events or rows.
    /// `MAX_PUSH_BYTES` in `dashboard/src/routes/api/machines/-handlers.ts` and
    /// `MAX_PUSH_EVENTS` in `dashboard/src/modules/observe-ingest/ingest.ts` are these
    /// numbers.
    pub(super) const CLOUD: Self = Self {
        rows: 1000,
        bytes: 256 * 1024,
    };
}

pub struct Pusher {
    url: Url,
    machine_id: MachineId,
    secret: ManagementSecret,
}

/// Read from machine.json before every send, so Cloud setting or clearing
/// the target and a Machine reset take effect without a restart. None until
/// Cloud sets a target, so the outbox only buffers.
pub fn push_target(data_dir: &Path) -> Option<Pusher> {
    let identity = store::identity(data_dir)?;
    Some(Pusher {
        url: identity.push_target?,
        machine_id: identity.id,
        secret: identity.management_secret,
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

pub(in crate::observe) fn unix_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_secs()).unwrap_or(i64::MAX)
        })
}

#[derive(Debug)]
pub(in crate::observe) struct Held {
    pub(super) body: Vec<u8>,
    pub(super) seqs: Vec<u64>,
}

/// What a 404 from Cloud tells a stream's shipper.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::observe) enum NotFound {
    /// Cloud has lost this Machine. The journal hears it now and then.
    Unheard,
    /// Cloud predates the stream. The shipper holds the batch quietly until it takes it.
    NotYetTaken,
}

pub(in crate::observe) trait Stream: Send + Sync + 'static {
    const NAME: &'static str;

    const ON_NOT_FOUND: NotFound = NotFound::Unheard;

    fn next(&self) -> io::Result<Option<Held>>;

    fn ack(&self, seqs: &[u64]) -> io::Result<()>;

    /// Deletes a batch Cloud will never take, counting its events dropped.
    fn refuse(&self, seqs: &[u64]) -> io::Result<()>;

    fn wake(&self) -> &Notify;

    fn salt_refresh(&self) -> io::Result<Option<Held>> {
        Ok(None)
    }
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
        let refresh = if self.asks_for_salts() {
            self.outbox.salt_refresh().unwrap_or_else(|error| {
                tracing::warn!(stream = S::NAME, %error, "cannot build a salt refresh");
                None
            })
        } else {
            None
        };
        let held = match refresh.map_or_else(|| self.outbox.next(), |held| Ok(Some(held))) {
            Ok(Some(held)) => held,
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
            StatusCode::NOT_FOUND if S::ON_NOT_FOUND == NotFound::NotYetTaken => {
                tracing::debug!(
                    stream = S::NAME,
                    "Cloud does not take this stream yet; holding it"
                );
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
        self.salts.accept(&body, unix_seconds());
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

#[cfg(test)]
#[path = "push_tests.rs"]
mod tests;
