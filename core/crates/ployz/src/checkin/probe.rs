//! The probe window a waiting host opens when Cloud hands one out: it greets Cloud on the TCP
//! ports, trades a nonce with its peers over UDP, and once it ends reports what it heard on every
//! check-in.

use std::{
    fmt, fs, io,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    path::{Path, PathBuf},
    sync::{Mutex, PoisonError},
    time::Duration,
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use futures_util::future::join_all;
use hmac::{Hmac, KeyInit as _, Mac as _};
use rustix::io::Errno;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use socket2::{Domain, Type};
use tokio::{
    io::AsyncWriteExt as _,
    net::{TcpListener, UdpSocket},
    task::JoinHandle,
    time::{Instant, MissedTickBehavior},
};
use tokio_util::sync::CancellationToken;

use super::CheckinKey;

pub(super) const WINDOW_CAP: Duration = Duration::from_secs(10);
const RESEND: Duration = Duration::from_millis(500);
const GREETING_CHARS: usize = 22;
const CLOUD_READS_FOR: Duration = Duration::from_millis(500);
const DATAGRAM_BUFFER: usize = 2048;
const SOCKET_BACKOFF: Duration = Duration::from_millis(100);
const LISTEN_BACKLOG: i32 = 1024;

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
enum Port {
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

#[derive(Default)]
pub(super) struct Probes {
    finished: Option<Report>,
    window: Option<Window>,
    saved: Option<PathBuf>,
}

struct Window {
    task: JoinHandle<Option<Report>>,
    stop: CancellationToken,
}

impl Probes {
    /// Picks up the last window that ran to its end, since Cloud hands each one out once. A window
    /// cut short by a restart left nothing, so Cloud reads no report rather than a partial one.
    pub(super) fn load(saved: PathBuf) -> Self {
        let finished = fs::read(&saved)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Report>(&bytes).ok());
        Self {
            finished,
            window: None,
            saved: Some(saved),
        }
    }

    /// Waits for a window being cut short to finish saving first, so an older window can't put
    /// probe.json back after this one removes it.
    pub(super) async fn open(&mut self, handout: Handout, key: &CheckinKey) {
        self.close().await;
        self.finished = None;
        if let Some(saved) = &self.saved {
            let _ = fs::remove_file(saved);
        }
        let mut sockets = Vec::new();
        let mut bind_failed = Vec::new();
        for &port in &handout.listen {
            match bind(port) {
                Some(socket) => sockets.push(socket),
                None => bind_failed.push(port.to_string()),
            }
        }
        let stop = CancellationToken::new();
        let task = tokio::spawn(window(
            handout,
            key.public_hex().to_owned(),
            sockets,
            bind_failed,
            self.saved.clone(),
            stop.clone(),
        ));
        self.window = Some(Window { task, stop });
    }

    /// Only a window that has ended reports, so Cloud never stores what half a window heard.
    pub(super) async fn report(&mut self) -> Option<Report> {
        if self
            .window
            .as_ref()
            .is_some_and(|window| window.task.is_finished())
        {
            self.finish().await;
        }
        self.finished.clone()
    }

    /// Waits out the window, for a process that exits before the window would end.
    pub(super) async fn finish(&mut self) {
        if let Some(window) = &mut self.window {
            self.finished = (&mut window.task).await.ok().flatten();
            self.window = None;
        }
    }

    pub(super) async fn close(&mut self) {
        if let Some(window) = &self.window {
            window.stop.cancel();
        }
        self.finish().await;
    }
}

impl Drop for Probes {
    fn drop(&mut self) {
        if let Some(window) = &self.window {
            window.stop.cancel();
        }
    }
}

/// Returns the report of a window that ran out, after its sockets close and it is saved. A
/// stopped window returns nothing and saves nothing.
async fn window(
    handout: Handout,
    checkin_key: String,
    sockets: Vec<Socket>,
    bind_failed: Vec<String>,
    saved: Option<PathBuf>,
    stop: CancellationToken,
) -> Option<Report> {
    let opened = Instant::now();
    let report = Mutex::new(Report {
        handout: handout.handout.clone(),
        heard_from: Vec::new(),
        bind_failed,
    });
    {
        let mut listeners = Vec::new();
        let mut udp = Vec::new();
        for socket in sockets {
            match socket {
                Socket::Tcp(listener, port) => listeners.push((listener, port)),
                Socket::Udp(socket) => udp.push(socket),
            }
        }
        let greet = join_all(listeners.iter().map(|(listener, port)| {
            greet(
                listener,
                greeting(&handout.handout, &checkin_key, *port).into_bytes(),
            )
        }));
        let exchange = join_all(udp.iter().map(|socket| exchange(socket, &handout, &report)));
        tokio::select! {
            () = stop.cancelled() => return None,
            () = tokio::time::sleep_until(opened + handout.window()) => {}
            _ = async { tokio::join!(greet, exchange) } => {}
        }
    }
    let report = report.into_inner().unwrap_or_else(PoisonError::into_inner);
    if let Some(saved) = saved {
        let written = report.clone();
        let shown = saved.display().to_string();
        let outcome = tokio::task::spawn_blocking(move || save(&saved, &written)).await;
        if let Err(error) = outcome.map_err(io::Error::other).and_then(|saving| saving) {
            crate::ui::note(format!("Could not write {shown}: {error}"));
        }
    }
    Some(report)
}

fn save(path: &Path, report: &Report) -> io::Result<()> {
    #[cfg(test)]
    let _held = tests::SAVES.lock().unwrap_or_else(PoisonError::into_inner);
    let directory = path.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(directory)?;
    let mut file = tempfile::NamedTempFile::new_in(directory)?;
    serde_json::to_writer(&mut file, report)?;
    file.as_file().sync_all()?;
    file.persist(path).map_err(|error| error.error)?;
    Ok(())
}

enum Socket {
    Tcp(TcpListener, u16),
    Udp(UdpSocket),
}

fn bind(port: Port) -> Option<Socket> {
    match port {
        Port::Tcp(number) => {
            let socket = any_address(number, Type::STREAM)?;
            socket.listen(LISTEN_BACKLOG).ok()?;
            TcpListener::from_std(socket.into())
                .ok()
                .map(|listener| Socket::Tcp(listener, number))
        }
        Port::Udp(number) => {
            let socket = any_address(number, Type::DGRAM)?;
            UdpSocket::from_std(socket.into()).ok().map(Socket::Udp)
        }
    }
}

/// One socket for both families whatever `net.ipv6.bindv6only` says, or IPv4 alone on a host
/// without IPv6. Any other IPv6 failure, such as a port held in either family, fails the port.
fn any_address(number: u16, kind: Type) -> Option<socket2::Socket> {
    match bound(Domain::IPV6, number, kind) {
        Ok(socket) => Some(socket),
        Err(error) if without_ipv6(&error) => bound(Domain::IPV4, number, kind).ok(),
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

fn bound(domain: Domain, number: u16, kind: Type) -> io::Result<socket2::Socket> {
    #[cfg(test)]
    if domain == Domain::IPV6
        && let Some(errno) = tests::IPV6_FAILS_WITH.get()
    {
        return Err(io::Error::from_raw_os_error(errno));
    }
    let socket = socket2::Socket::new(domain, kind, None)?;
    let any = if domain == Domain::IPV6 {
        #[cfg(test)]
        if tests::ONLY_V6_BY_DEFAULT.get() {
            socket.set_only_v6(true)?;
        }
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
    encoded.truncate(GREETING_CHARS);
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
mod tests;
