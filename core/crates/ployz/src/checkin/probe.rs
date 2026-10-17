use std::{
    fmt,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    sync::{Arc, Mutex, PoisonError},
    time::Duration,
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use futures_util::future::join_all;
use hmac::{Hmac, KeyInit as _, Mac as _};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use tokio::{
    io::AsyncWriteExt as _,
    net::{TcpListener, UdpSocket},
    task::JoinHandle,
    time::{Instant, MissedTickBehavior},
};

use super::CheckinKey;

pub(super) const WINDOW_CAP: Duration = Duration::from_secs(10);
const BIND_AFTER_HANDOUT: Duration = Duration::from_secs(1);
const RESEND: Duration = Duration::from_millis(500);
const GREETING_CHARS: usize = 22;
const CLOUD_READS_FOR: Duration = Duration::from_millis(500);
const DATAGRAM_BUFFER: usize = 2048;

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

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Report {
    handout: String,
    heard_from: Vec<String>,
    bind_failed: Vec<String>,
}

#[derive(Default)]
pub(super) struct Probes {
    report: Option<Arc<Mutex<Report>>>,
    window: Option<JoinHandle<()>>,
}

impl Probes {
    pub(super) async fn open(&mut self, handout: Handout, key: &CheckinKey) {
        self.close().await;
        let report = Arc::new(Mutex::new(Report {
            handout: handout.handout.clone(),
            heard_from: Vec::new(),
            bind_failed: Vec::new(),
        }));
        self.window = Some(tokio::spawn(window(
            handout,
            key.public_hex().to_owned(),
            Arc::clone(&report),
        )));
        self.report = Some(report);
    }

    pub(super) fn report(&self) -> Option<Report> {
        self.report.as_ref().map(|report| {
            report
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone()
        })
    }

    pub(super) async fn close(&mut self) {
        if let Some(window) = self.window.take() {
            window.abort();
            let _ = window.await;
        }
    }
}

impl Drop for Probes {
    fn drop(&mut self) {
        if let Some(window) = &self.window {
            window.abort();
        }
    }
}

async fn window(handout: Handout, checkin_key: String, report: Arc<Mutex<Report>>) {
    let opened = Instant::now();
    let bind_by = opened + BIND_AFTER_HANDOUT;
    let bound = join_all(handout.listen.iter().map(|&port| bind(port, bind_by))).await;
    let mut listeners = Vec::new();
    let mut udp = Vec::new();
    let mut failed = Vec::new();
    for (port, socket) in handout.listen.iter().zip(bound) {
        match socket {
            Some(Socket::Tcp(listener, number)) => listeners.push((listener, number)),
            Some(Socket::Udp(socket)) => udp.push(socket),
            None => failed.push(port.to_string()),
        }
    }
    report
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .bind_failed = failed;
    let greet = join_all(listeners.iter().map(|(listener, port)| {
        greet(
            listener,
            greeting(&handout.handout, &checkin_key, *port).into_bytes(),
        )
    }));
    let exchange = join_all(udp.iter().map(|socket| exchange(socket, &handout, &report)));
    tokio::select! {
        () = tokio::time::sleep_until(opened + handout.window()) => {}
        _ = async { tokio::join!(greet, exchange) } => {}
    }
}

enum Socket {
    Tcp(TcpListener, u16),
    Udp(UdpSocket),
}

async fn bind(port: Port, by: Instant) -> Option<Socket> {
    let any = |number| {
        [
            SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), number),
            SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), number),
        ]
    };
    let bound = async {
        match port {
            Port::Tcp(number) => TcpListener::bind(any(number).as_slice())
                .await
                .ok()
                .map(|listener| Socket::Tcp(listener, number)),
            Port::Udp(number) => UdpSocket::bind(any(number).as_slice())
                .await
                .ok()
                .map(Socket::Udp),
        }
    };
    tokio::time::timeout_at(by, bound).await.ok().flatten()
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
        if let Ok((mut stream, _)) = listener.accept().await {
            let _ = tokio::time::timeout(CLOUD_READS_FOR, stream.write_all(&greeting)).await;
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
                let Ok((length, source)) = received else { continue };
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
