//! The probe window a waiting host opens when Cloud hands one out: it greets Cloud on the TCP
//! ports, trades a nonce with its peers over UDP, and once it ends reports what it heard on every
//! check-in.

use std::{
    collections::BTreeMap,
    fmt, fs, io,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, SystemTime},
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use futures_util::{StreamExt as _, stream::FuturesUnordered};
use hmac::{Hmac, KeyInit as _, Mac as _};
use ployz_core::cloud_contract::{
    CHECKIN_WAIT_SECONDS, PROBE_ATTEMPT_EVERY_MS, PROBE_FIRST_ATTEMPT_MS, PROBE_GREETING_LENGTH,
    PROBE_GREETING_READ_MS, PROBE_WINDOW_MS,
};
use rustix::io::Errno;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use socket2::{Domain, Type};
use tokio::{
    io::AsyncWriteExt as _,
    net::{TcpListener, UdpSocket},
    sync::mpsc,
    task::JoinHandle,
    time::{Instant, MissedTickBehavior},
};
use tokio_util::sync::CancellationToken;

use super::CheckinKey;

pub(super) const WINDOW_CAP: Duration = Duration::from_millis(PROBE_WINDOW_MS);
const RESEND: Duration = Duration::from_millis(500);
const CLOUD_READS_FOR: Duration = Duration::from_millis(PROBE_GREETING_READ_MS);
const DATAGRAM_BUFFER: usize = 2048;
const SOCKET_BACKOFF: Duration = Duration::from_millis(100);
const LISTEN_BACKLOG: i32 = 1024;
/// Cloud settles a probe this long after its window ends.
const SETTLE_GRACE: Duration = Duration::from_secs(30);
/// How soon a failed pass tries again while Cloud still waits on a report: its own check-in pace.
const REPORT_RETRY: Duration = Duration::from_secs(CHECKIN_WAIT_SECONDS);

/// The newest window to claim each saved path, whichever `Probes` opened it. A writer saves only
/// while its window is still the newest, so an older report never lands over a newer window.
static NEWEST: Mutex<BTreeMap<PathBuf, u64>> = Mutex::new(BTreeMap::new());
static WINDOWS: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(super) struct Handout {
    nonce: String,
    handout: String,
    window_ms: u64,
    listen: Vec<Port>,
    send: Vec<Target>,
}

impl Handout {
    fn window(&self) -> Duration {
        Duration::from_millis(self.window_ms).min(WINDOW_CAP)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(try_from = "String")]
pub(crate) enum Port {
    Tcp(u16),
    Udp(u16),
}

impl TryFrom<String> for Port {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        let parsed = match value.split_once(':') {
            Some(("tcp", port)) => port.parse().ok().map(Self::Tcp),
            Some(("udp", port)) => port.parse().ok().map(Self::Udp),
            _ => None,
        };
        parsed.ok_or_else(|| format!("not a probe port: {value}"))
    }
}

impl fmt::Display for Port {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Tcp(port) => write!(formatter, "tcp:{port}"),
            Self::Udp(port) => write!(formatter, "udp:{port}"),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
struct Target {
    target: IpAddr,
    port: u16,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Report {
    handout: String,
    heard_from: Vec<String>,
    bind_failed: Vec<String>,
}

/// What a window leaves to its host: the sockets it opens, and the pace it binds and saves at.
/// Every method does what the host does, so only a test overrides one, to fail a bind, stall it or
/// shorten the schedule.
pub(crate) trait Binder: Send + Sync + 'static {
    /// A fresh socket, before the window configures and binds it.
    fn open(&self, domain: Domain, kind: Type) -> io::Result<socket2::Socket> {
        socket2::Socket::new(domain, kind, None)
    }

    /// How long before `window` ends its binds must be done, in place of D8's schedule.
    fn lead(&self, _window: Duration) -> Option<Duration> {
        None
    }

    /// Runs on the runtime thread as `open` hands a bind out. What it returns drops when the bind
    /// returns.
    fn handed_out(&self, _port: Port) -> Box<dyn Send> {
        Box::new(())
    }

    /// Runs on the bind's thread as it starts, before it checks its deadline.
    fn began(&self, _port: Port) {}

    /// Runs on the bind's thread before it opens a socket.
    fn binding(&self, _port: Port) {}

    /// Runs `write`, which saves the report.
    fn saving(&self, write: &mut dyn FnMut() -> io::Result<()>) -> io::Result<()> {
        write()
    }
}

/// The host's own sockets.
pub(crate) struct Sockets;

impl Binder for Sockets {}

pub(super) struct Probes {
    finished: Option<Report>,
    window: Option<Window>,
    saving: Option<JoinHandle<()>>,
    saved: Option<PathBuf>,
    owed_until: Option<Instant>,
    binder: Arc<dyn Binder>,
}

impl Default for Probes {
    fn default() -> Self {
        Self::new(Arc::new(Sockets))
    }
}

struct Window {
    task: JoinHandle<Option<Ended>>,
    stop: CancellationToken,
}

impl Drop for Window {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

struct Ended {
    report: Report,
    saving: Option<JoinHandle<()>>,
}

impl Probes {
    /// Picks up the last window that ran to its end, since Cloud hands each one out once. A window
    /// cut short by a restart left nothing, so Cloud reads no report rather than a partial one.
    pub(super) fn new(binder: Arc<dyn Binder>) -> Self {
        Self {
            finished: None,
            window: None,
            saving: None,
            saved: None,
            owed_until: None,
            binder,
        }
    }

    pub(super) fn load(saved: PathBuf, binder: Arc<dyn Binder>) -> Self {
        let finished = fs::read(&saved)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Report>(&bytes).ok());
        let owed_until = finished.as_ref().and_then(|_| settled_at(&saved));
        Self {
            finished,
            owed_until,
            saved: Some(saved),
            ..Self::new(binder)
        }
    }

    /// Counts the bind budget and window from before the check-in request was sent, so delayed
    /// replies cannot start listeners after Cloud's last attempt. Never waits on an older save.
    pub(super) async fn open(&mut self, handout: Handout, key: &CheckinKey, sent: Instant) {
        let expires = sent + handout.window();
        let deadline = sent + bind_deadline(self.binder.as_ref(), handout.window());
        self.close().await;
        self.finished = None;
        self.saving = None;
        self.owed_until = Some(expires + SETTLE_GRACE);
        let saved = self.saved.clone().map(|saved| (claim(&saved), saved));
        let ports = handout.listen.clone();
        let (bound, sockets) = mpsc::unbounded_channel();
        let stop = CancellationToken::new();
        let task = tokio::spawn(window(
            handout,
            key.public_hex().to_owned(),
            sockets,
            saved,
            stop.clone(),
            expires,
            Arc::clone(&self.binder),
        ));
        self.window = Some(Window {
            task,
            stop: stop.clone(),
        });
        for port in ports {
            if Instant::now() >= deadline || stop.is_cancelled() {
                break;
            }
            let binder = Arc::clone(&self.binder);
            let handed_out = binder.handed_out(port);
            let binding_stop = stop.clone();
            let binding = tokio::task::spawn_blocking(move || {
                let _handed_out = handed_out;
                binder.began(port);
                if Instant::now() >= deadline || binding_stop.is_cancelled() {
                    return None;
                }
                bind(binder.as_ref(), port)
                    .filter(|_| Instant::now() < deadline && !binding_stop.is_cancelled())
            });
            if let Ok(Ok(Some(socket))) = tokio::time::timeout_at(deadline, binding).await {
                let _ = bound.send((port, socket));
            }
        }
    }

    /// Only a window that has ended reports, so Cloud never stores what half a window heard.
    pub(super) async fn report(&mut self) -> Option<Report> {
        if self
            .window
            .as_ref()
            .is_some_and(|window| window.task.is_finished())
        {
            self.harvest().await;
        }
        self.finished.clone()
    }

    /// Waits out the window and its save, for a process that exits before the window would end.
    pub(super) async fn finish(&mut self) {
        self.harvest().await;
        if let Some(saving) = self.saving.take() {
            let _ = saving.await;
        }
    }

    /// Never waits on a save, which carries on and lands unless a newer window claims the path.
    pub(super) async fn close(&mut self) {
        if let Some(window) = &self.window {
            window.stop.cancel();
        }
        self.harvest().await;
    }

    /// A failed pass tries again within Cloud's settle, while it still waits on this report.
    pub(super) fn retry_after(&self, after: Duration) -> Duration {
        match self.owed_until {
            Some(until) if Instant::now() < until => after.min(REPORT_RETRY),
            _ => after,
        }
    }

    pub(super) fn reported(&mut self) {
        self.owed_until = None;
    }

    async fn harvest(&mut self) {
        if let Some(window) = &mut self.window {
            let ended = (&mut window.task).await.ok().flatten();
            self.window = None;
            if let Some(ended) = ended {
                self.finished = Some(ended.report);
                self.saving = ended.saving;
            }
        }
    }
}

/// D8 hands out ten-second windows and tries TCP at 1, 2.5, 4, 5.5, 7 and 8.5 seconds.
/// Bind before its last attempt with time for the greeting. Windows without an attempt cannot bind.
fn bind_deadline(binder: &dyn Binder, window: Duration) -> Duration {
    if let Some(lead) = binder.lead(window) {
        return window.saturating_sub(lead);
    }
    let first = Duration::from_millis(PROBE_FIRST_ATTEMPT_MS);
    let every = Duration::from_millis(PROBE_ATTEMPT_EVERY_MS);
    if window <= first {
        return Duration::ZERO;
    }
    let attempts = (window - first - Duration::from_nanos(1)).as_nanos() / every.as_nanos();
    let last = first + every * u32::try_from(attempts).expect("window is capped at ten seconds");
    (window * 7 / 10)
        .min(last.saturating_sub(CLOUD_READS_FOR))
        .min(window.saturating_sub(every + CLOUD_READS_FOR))
}

/// When Cloud stops waiting on a saved report, from the file's mtime, which is when its window
/// ended. This is best effort across wall-clock changes and reboots, which lose `open`'s deadline.
fn settled_at(saved: &Path) -> Option<Instant> {
    let ended = fs::metadata(saved).and_then(|meta| meta.modified()).ok()?;
    let left = (ended + SETTLE_GRACE)
        .duration_since(SystemTime::now())
        .ok()?;
    Some(Instant::now() + left.min(SETTLE_GRACE))
}

/// Makes `saved` belong to a new window: from here no older window's writer can land on it.
fn claim(saved: &Path) -> u64 {
    let mut newest = NEWEST.lock().unwrap_or_else(PoisonError::into_inner);
    let generation = WINDOWS.fetch_add(1, Ordering::Relaxed);
    newest.insert(saved.to_owned(), generation);
    let _ = fs::remove_file(saved);
    generation
}

/// Returns the report of a window that ran out once its sockets close, with its save under way.
/// A stopped window returns nothing and saves nothing.
async fn window(
    handout: Handout,
    checkin_key: String,
    mut sockets: mpsc::UnboundedReceiver<(Port, Socket)>,
    saved: Option<(u64, PathBuf)>,
    stop: CancellationToken,
    expires: Instant,
    binder: Arc<dyn Binder>,
) -> Option<Ended> {
    let report = Mutex::new(Report {
        handout: handout.handout.clone(),
        heard_from: Vec::new(),
        bind_failed: handout.listen.iter().map(ToString::to_string).collect(),
    });
    {
        let mut serving = FuturesUnordered::new();
        let mut binding = true;
        loop {
            tokio::select! {
                biased;
                () = tokio::time::sleep_until(expires) => break,
                () = stop.cancelled() => return None,
                socket = sockets.recv(), if binding => {
                    let Some((port, socket)) = socket else {
                        binding = false;
                        continue;
                    };
                    report.lock().unwrap_or_else(PoisonError::into_inner)
                        .bind_failed.retain(|failed| failed != &port.to_string());
                    let handout = &handout;
                    let checkin_key = &checkin_key;
                    let report = &report;
                    serving.push(async move {
                        match socket {
                            Socket::Tcp(listener, port) => greet(
                                &listener,
                                greeting(&handout.handout, checkin_key, port).into_bytes(),
                            ).await,
                            Socket::Udp(socket) => exchange(&socket, handout, report).await,
                        }
                    });
                }
                _ = serving.next(), if !serving.is_empty() => {}
            }
        }
    }
    drop(sockets);
    let report = report.into_inner().unwrap_or_else(PoisonError::into_inner);
    let saving = saved.map(|(generation, saved)| {
        let written = report.clone();
        tokio::task::spawn_blocking(move || {
            if let Err(error) = save(binder.as_ref(), &saved, &written, generation) {
                crate::ui::note(format!("Could not write {}: {error}", saved.display()));
            }
        })
    });
    Some(Ended { report, saving })
}

fn save(binder: &dyn Binder, path: &Path, report: &Report, generation: u64) -> io::Result<()> {
    binder.saving(&mut || write(path, report, generation))
}

fn write(path: &Path, report: &Report, generation: u64) -> io::Result<()> {
    let directory = path.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(directory)?;
    let mut file = tempfile::NamedTempFile::new_in(directory)?;
    serde_json::to_writer(&mut file, report)?;
    file.as_file().sync_all()?;
    let newest = NEWEST.lock().unwrap_or_else(PoisonError::into_inner);
    if newest.get(path) == Some(&generation) {
        file.persist(path).map_err(|error| error.error)?;
    }
    Ok(())
}

enum Socket {
    Tcp(TcpListener, u16),
    Udp(UdpSocket),
}

fn bind(binder: &dyn Binder, port: Port) -> Option<Socket> {
    binder.binding(port);
    match port {
        Port::Tcp(number) => {
            let socket = any_address(binder, number, Type::STREAM)?;
            socket.listen(LISTEN_BACKLOG).ok()?;
            TcpListener::from_std(socket.into())
                .ok()
                .map(|listener| Socket::Tcp(listener, number))
        }
        Port::Udp(number) => {
            let socket = any_address(binder, number, Type::DGRAM)?;
            UdpSocket::from_std(socket.into()).ok().map(Socket::Udp)
        }
    }
}

/// One socket for both families whatever `net.ipv6.bindv6only` says, or IPv4 alone on a host
/// without IPv6. Any other IPv6 failure, such as a port held in either family, fails the port.
fn any_address(binder: &dyn Binder, number: u16, kind: Type) -> Option<socket2::Socket> {
    match bound(binder, Domain::IPV6, number, kind) {
        Ok(socket) => Some(socket),
        Err(error) if without_ipv6(&error) => bound(binder, Domain::IPV4, number, kind).ok(),
        Err(_) => None,
    }
}

fn without_ipv6(error: &io::Error) -> bool {
    Errno::from_io_error(error).is_some_and(|errno| {
        [
            Errno::AFNOSUPPORT,
            Errno::ADDRNOTAVAIL,
            Errno::PROTONOSUPPORT,
        ]
        .contains(&errno)
    })
}

fn bound(
    binder: &dyn Binder,
    domain: Domain,
    number: u16,
    kind: Type,
) -> io::Result<socket2::Socket> {
    let socket = binder.open(domain, kind)?;
    let any = if domain == Domain::IPV6 {
        socket.set_only_v6(false)?;
        IpAddr::V6(Ipv6Addr::UNSPECIFIED)
    } else {
        IpAddr::V4(Ipv4Addr::UNSPECIFIED)
    };
    if kind == Type::STREAM {
        socket.set_reuse_address(true)?;
    }
    socket.set_nonblocking(true)?;
    socket.bind(&SocketAddr::new(any, number).into())?;
    Ok(socket)
}

fn greeting(handout: &str, checkin_key: &str, port: u16) -> String {
    let mut mac =
        Hmac::<Sha256>::new_from_slice(handout.as_bytes()).expect("HMAC takes a key of any length");
    mac.update(format!("ployz-probe-tcp:{checkin_key}:{port}").as_bytes());
    let mut encoded = URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes());
    encoded.truncate(PROBE_GREETING_LENGTH);
    encoded
}

async fn greet(listener: &TcpListener, greeting: Vec<u8>) {
    loop {
        match listener.accept().await {
            Ok((mut stream, _)) => {
                let _ = tokio::time::timeout(CLOUD_READS_FOR, stream.write_all(&greeting)).await;
            }
            // Out of descriptors, accept fails at once: back off rather than spin.
            Err(_) => tokio::time::sleep(SOCKET_BACKOFF).await,
        }
    }
}

async fn exchange(socket: &UdpSocket, handout: &Handout, report: &Mutex<Report>) {
    let v6 = socket.local_addr().is_ok_and(|local| local.is_ipv6());
    let targets: Vec<SocketAddr> = handout
        .send
        .iter()
        .map(|peer| {
            let ip = match peer.target {
                IpAddr::V4(ip) if v6 => IpAddr::V6(ip.to_ipv6_mapped()),
                IpAddr::V4(_) | IpAddr::V6(_) => peer.target,
            };
            SocketAddr::new(ip, peer.port)
        })
        .collect();
    let mut resend = tokio::time::interval(RESEND);
    resend.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut buffer = [0_u8; DATAGRAM_BUFFER];
    loop {
        tokio::select! {
            _ = resend.tick() => {
                for target in &targets {
                    let _ = socket.send_to(handout.nonce.as_bytes(), target).await;
                }
            }
            received = socket.recv_from(&mut buffer) => {
                let Ok((length, source)) = received else {
                    tokio::time::sleep(SOCKET_BACKOFF).await;
                    continue;
                };
                if buffer.get(..length) != Some(handout.nonce.as_bytes()) {
                    continue;
                }
                let source = source.ip().to_canonical().to_string();
                let mut report = report.lock().unwrap_or_else(PoisonError::into_inner);
                if !report.heard_from.contains(&source) {
                    report.heard_from.push(source);
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "probe_tests.rs"]
pub(super) mod tests;
