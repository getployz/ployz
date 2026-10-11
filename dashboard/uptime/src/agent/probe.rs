//! One probe: resolve once, refuse the target if any answer is not global, dial one pinned vetted
//! address, TLS with SNI, and stop at the HTTP/1.1 response head. Every phase is timed and every
//! failure is an [`Outcome`], never an error.

use std::collections::HashMap;
use std::error::Error;
use std::future::{Future, poll_fn};
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::pin::pin;
use std::sync::{Arc, Mutex, PoisonError};
use std::task::Poll;
use std::time::{Duration, Instant};

use bytes::Bytes;
use http::header::{ACCEPT, CONNECTION, HOST, USER_AGENT};
use http_body_util::Empty;
use hyper_util::rt::TokioIo;
use rustls::crypto::CryptoProvider;
use rustls::pki_types::ServerName;
use rustls::{ClientConfig, RootCertStore};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio::sync::Semaphore;
use tokio_rustls::TlsConnector;

use crate::domain::{
    AgentName, Detail, Host, HttpCheckError, Job, NotProbed, Observation, Outcome, Phase, Scheme,
    Target, Timings, UnixMs,
};

const USER_AGENT_VALUE: &str = "PloyzUptime/1 (+https://ployz.dev/uptime)";

#[derive(Clone, Copy, Debug)]
pub struct ProbeConfig {
    pub max_inflight: usize,
    /// One dead Machine can't eat the agent.
    pub per_address: usize,
    /// `UPTIME_IPV6`. Railway egress is IPv4.
    pub allow_ipv6: bool,
    /// Status line plus headers. The body is never read.
    pub header_limit: usize,
    pub dns_timeout: Duration,
    pub connect_timeout: Duration,
    pub tls_timeout: Duration,
}

impl Default for ProbeConfig {
    fn default() -> ProbeConfig {
        ProbeConfig {
            max_inflight: 16_384,
            per_address: 64,
            allow_ipv6: false,
            header_limit: 16 * 1024,
            dns_timeout: Duration::from_secs(5),
            connect_timeout: Duration::from_secs(5),
            tls_timeout: Duration::from_secs(5),
        }
    }
}

/// Where a hostname's answers come from. Production asks the system resolver; tests say what a
/// name resolves to.
pub trait Resolve: Send + Sync {
    fn resolve(&self, host: &str) -> impl Future<Output = io::Result<Vec<IpAddr>>> + Send;
}

pub struct SystemResolver;

impl Resolve for SystemResolver {
    async fn resolve(&self, host: &str) -> io::Result<Vec<IpAddr>> {
        Ok(tokio::net::lookup_host((host, 0))
            .await?
            .map(|a| a.ip())
            .collect())
    }
}

/// Owns the TLS config and the in-flight limits. One method, and it never errors.
pub struct Prober<R = SystemResolver> {
    resolver: R,
    tls: TlsConnector,
    inflight: Semaphore,
    per_address: PerAddress,
    config: ProbeConfig,
    agent: AgentName,
    /// Lets tests aim the HTTP path at local servers. The field and its check don't exist outside
    /// `cfg(test)`, so no production configuration can turn it on.
    #[cfg(test)]
    pub(crate) loopback_ok: bool,
    /// Lets tests redirect a loopback dial to an ephemeral port while the check URL keeps port 80.
    #[cfg(test)]
    pub(crate) loopback_port: Option<u16>,
}

impl Prober {
    /// # Errors
    /// The crypto provider supports none of rustls's safe default protocol versions.
    pub fn new(
        config: ProbeConfig,
        crypto: Arc<CryptoProvider>,
        agent: AgentName,
    ) -> Result<Prober, rustls::Error> {
        let roots = RootCertStore {
            roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
        };
        Ok(Prober::with(
            SystemResolver,
            client_config(crypto, roots)?,
            config,
            agent,
        ))
    }
}

pub(crate) fn client_config(
    crypto: Arc<CryptoProvider>,
    roots: RootCertStore,
) -> Result<ClientConfig, rustls::Error> {
    let mut tls = ClientConfig::builder_with_provider(crypto)
        .with_safe_default_protocol_versions()?
        .with_root_certificates(roots)
        .with_no_client_auth();
    tls.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(tls)
}

impl<R: Resolve> Prober<R> {
    pub(crate) fn with(
        resolver: R,
        tls: ClientConfig,
        config: ProbeConfig,
        agent: AgentName,
    ) -> Self {
        Prober {
            resolver,
            tls: TlsConnector::from(Arc::new(tls)),
            inflight: Semaphore::new(config.max_inflight),
            per_address: PerAddress {
                cap: config.per_address,
                inflight: Mutex::new(HashMap::new()),
            },
            config,
            agent,
            #[cfg(test)]
            loopback_ok: false,
            #[cfg(test)]
            loopback_port: None,
        }
    }

    /// Runs one probe within `job.timeout`. `started_at` is the agent's corrected clock at the
    /// start, read by the caller.
    pub async fn probe(&self, job: &Job, started_at: UnixMs) -> Observation {
        let began = Instant::now();
        let mut run = Run {
            deadline: began + job.timeout,
            timings: Timings::default(),
            remote_ip: None,
            cert_not_after: None,
        };
        let outcome = match self.attempt(job, &mut run).await {
            Ok(status) => Outcome::Http { status },
            Err(outcome) => outcome,
        };
        run.timings.total_ms = millis(began.elapsed());
        Observation {
            key: job.key.clone(),
            started_at,
            outcome,
            timings: run.timings,
            remote_ip: run.remote_ip,
            cert_not_after: run.cert_not_after,
            agent: self.agent.clone(),
        }
    }

    async fn attempt(&self, job: &Job, run: &mut Run) -> Result<u16, Outcome> {
        let Ok(_permit) = self.inflight.try_acquire() else {
            return Err(Outcome::NotProbed(NotProbed::Saturated));
        };
        let target = &job.target;
        let answers = match &target.host {
            Host::Ip(ip) => vec![*ip],
            Host::Name(name) => {
                let answers = run
                    .phase(
                        Phase::Dns,
                        Some(self.config.dns_timeout),
                        self.resolver.resolve(name),
                    )
                    .await?;
                if answers.is_empty() {
                    return Err(unreachable(
                        Phase::Dns,
                        HttpCheckError::Other,
                        "no addresses",
                    ));
                }
                answers
            }
        };
        // From here on the pinned address is the only one used. Nothing resolves again.
        let pinned = self.pin(answers, job.address)?;
        let Some(_claim) = self.per_address.claim(pinned) else {
            return Err(Outcome::NotProbed(NotProbed::Saturated));
        };
        run.remote_ip = Some(pinned);
        let tcp = run
            .phase(
                Phase::Connect,
                Some(self.config.connect_timeout),
                TcpStream::connect(SocketAddr::new(pinned, self.dial_port(pinned, target.port))),
            )
            .await?;
        match target.scheme {
            Scheme::Http => self.request(run, target, tcp).await,
            Scheme::Https => {
                let name = server_name(&target.host)?;
                let tls = run
                    .phase(
                        Phase::Tls,
                        Some(self.config.tls_timeout),
                        self.tls.connect(name, tcp),
                    )
                    .await?;
                run.cert_not_after = leaf_not_after(&tls);
                self.request(run, target, tls).await
            }
        }
    }

    /// Vets every answer, then picks one by `index`. One non-global answer refuses the whole
    /// target: a rebinding name mixes good and bad answers and hopes the good one gets vetted.
    fn pin(&self, answers: Vec<IpAddr>, index: u32) -> Result<IpAddr, Outcome> {
        let mut dialable = Vec::with_capacity(answers.len());
        for ip in answers {
            let ip = ip.to_canonical();
            if !self.vetted(ip) {
                return Err(Outcome::NotProbed(NotProbed::PrivateAddress));
            }
            if ip.is_ipv4() || self.config.allow_ipv6 {
                dialable.push(ip);
            }
        }
        // The resolver's order varies, so the index means the same address on every agent.
        dialable.sort_unstable();
        dialable.dedup();
        let count = dialable.len().max(1);
        let index = usize::try_from(index).unwrap_or(0) % count;
        dialable
            .get(index)
            .copied()
            .ok_or(Outcome::NotProbed(NotProbed::PrivateAddress))
    }

    #[cfg(not(test))]
    fn dial_port(&self, _ip: IpAddr, port: u16) -> u16 {
        port
    }

    #[cfg(test)]
    fn dial_port(&self, ip: IpAddr, port: u16) -> u16 {
        match self.loopback_port {
            Some(local) if self.loopback_ok && ip.is_loopback() => local,
            Some(_) | None => port,
        }
    }

    fn vetted(&self, ip: IpAddr) -> bool {
        #[cfg(test)]
        if self.loopback_ok && ip.is_loopback() {
            return true;
        }
        is_global(ip)
    }

    async fn request<S>(&self, run: &mut Run, target: &Target, io: S) -> Result<u16, Outcome>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let request = http::Request::get(target.path_and_query.as_str())
            .header(HOST, host_header(target))
            .header(USER_AGENT, USER_AGENT_VALUE)
            .header(ACCEPT, "*/*")
            .header(CONNECTION, "close")
            .body(Empty::<Bytes>::new())
            .map_err(|e| failed(Phase::Request, &e))?;
        // hyper panics below its minimum buffer.
        let header_limit = self.config.header_limit.max(8192);
        run.phase(Phase::Request, None, async move {
            let (mut sender, connection) = hyper::client::conn::http1::Builder::new()
                .max_buf_size(header_limit)
                // No header line is shorter than 4 bytes, so the byte limit alone decides.
                .max_headers(header_limit / 4)
                .handshake(TokioIo::new(io))
                .await?;
            let mut connection = pin!(connection);
            let mut response = pin!(sender.send_request(request));
            // The response resolves at the head. Returning drops it and the connection
            // unread, so a body that never ends can't hold the probe.
            tokio::select! {
                biased;
                head = &mut response => head.map(|h| h.status().as_u16()),
                ended = &mut connection => match ended {
                    // One connection poll can read the head and then the server's abrupt close
                    // (no TLS close_notify), so a head already handed over still wins.
                    Err(e) => match poll_fn(|cx| Poll::Ready(response.as_mut().poll(cx))).await {
                        Poll::Ready(Ok(head)) => Ok(head.status().as_u16()),
                        Poll::Ready(Err(_)) | Poll::Pending => Err(e),
                    },
                    Ok(()) => response.await.map(|h| h.status().as_u16()),
                },
            }
        })
        .await
    }
}

/// The state one probe accumulates while it runs.
struct Run {
    deadline: Instant,
    timings: Timings,
    remote_ip: Option<IpAddr>,
    cert_not_after: Option<i64>,
}

impl Run {
    /// Runs one phase under `cap`, itself cut to what is left of the job's timeout, and records
    /// its duration.
    async fn phase<T, E>(
        &mut self,
        phase: Phase,
        cap: Option<Duration>,
        work: impl Future<Output = Result<T, E>>,
    ) -> Result<T, Outcome>
    where
        E: Error + 'static,
    {
        let left = self.deadline.saturating_duration_since(Instant::now());
        let budget = cap.map_or(left, |cap| cap.min(left));
        let began = Instant::now();
        let result = tokio::time::timeout(budget, work).await;
        let spent = millis(began.elapsed());
        match phase {
            Phase::Dns => self.timings.dns_ms = spent,
            Phase::Connect => self.timings.connect_ms = spent,
            Phase::Tls => self.timings.tls_ms = spent,
            Phase::Request => self.timings.ttfb_ms = spent,
        }
        match result {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(e)) => Err(failed(phase, &e)),
            Err(_) => Err(unreachable(
                phase,
                HttpCheckError::TimedOut,
                &format!(
                    "{} timed out after {} ms",
                    phase_name(phase),
                    budget.as_millis()
                ),
            )),
        }
    }
}

fn failed(phase: Phase, error: &(dyn Error + 'static)) -> Outcome {
    unreachable(phase, classify(error), &chain(error))
}

fn unreachable(phase: Phase, error: HttpCheckError, detail: &str) -> Outcome {
    Outcome::Unreachable {
        phase,
        error,
        detail: Detail::new(detail),
    }
}

/// Mirrors ployzd's `unreachable_class` for health checks, so both probers name the same failure
/// the same way. Timeouts the probe imposes itself are classified where they fire.
fn classify(error: &(dyn Error + 'static)) -> HttpCheckError {
    let mut source = Some(error);
    while let Some(cause) = source {
        if let Some(io) = cause.downcast_ref::<io::Error>() {
            if io.kind() == io::ErrorKind::ConnectionRefused {
                return HttpCheckError::ConnectionRefused;
            }
            if io.kind() == io::ErrorKind::TimedOut {
                return HttpCheckError::TimedOut;
            }
        }
        source = cause.source();
    }
    HttpCheckError::Other
}

/// The error and its causes on one line. A cause whose text is already there adds nothing.
fn chain(error: &(dyn Error + 'static)) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        let next = cause.to_string();
        if !text.contains(&next) {
            text.push_str(": ");
            text.push_str(&next);
        }
        source = cause.source();
    }
    text
}

fn phase_name(phase: Phase) -> &'static str {
    match phase {
        Phase::Dns => "dns",
        Phase::Connect => "connect",
        Phase::Tls => "tls",
        Phase::Request => "response",
    }
}

fn server_name(host: &Host) -> Result<ServerName<'static>, Outcome> {
    match host {
        Host::Name(name) => ServerName::try_from(name.clone()).map_err(|e| failed(Phase::Tls, &e)),
        Host::Ip(ip) => Ok(ServerName::IpAddress((*ip).into())),
    }
}

fn host_header(target: &Target) -> String {
    let host = match &target.host {
        Host::Name(name) => name.clone(),
        Host::Ip(IpAddr::V6(ip)) => format!("[{ip}]"),
        Host::Ip(IpAddr::V4(ip)) => ip.to_string(),
    };
    let default_port = match target.scheme {
        Scheme::Http => 80,
        Scheme::Https => 443,
    };
    if target.port == default_port {
        host
    } else {
        format!("{host}:{}", target.port)
    }
}

fn leaf_not_after<S>(tls: &tokio_rustls::client::TlsStream<S>) -> Option<i64> {
    let leaf = tls.get_ref().1.peer_certificates()?.first()?;
    let (_, cert) = x509_parser::parse_x509_certificate(leaf).ok()?;
    Some(cert.validity().not_after.timestamp())
}

fn millis(duration: Duration) -> u32 {
    u32::try_from(duration.as_millis()).unwrap_or(u32::MAX)
}

/// In-flight probes per pinned address. Entries go when their count reaches zero.
struct PerAddress {
    cap: usize,
    inflight: Mutex<HashMap<IpAddr, usize>>,
}

impl PerAddress {
    fn claim(&self, ip: IpAddr) -> Option<Claim<'_>> {
        let mut inflight = self.inflight.lock().unwrap_or_else(PoisonError::into_inner);
        let count = inflight.get(&ip).copied().unwrap_or(0);
        if count >= self.cap {
            return None;
        }
        inflight.insert(ip, count + 1);
        Some(Claim { owner: self, ip })
    }
}

struct Claim<'a> {
    owner: &'a PerAddress,
    ip: IpAddr,
}

impl Drop for Claim<'_> {
    fn drop(&mut self) {
        let mut inflight = self
            .owner
            .inflight
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(count) = inflight.get_mut(&self.ip) {
            *count -= 1;
            if *count == 0 {
                inflight.remove(&self.ip);
            }
        }
    }
}

/// IPv4 ranges that are never a public endpoint: this network, private, CGNAT, loopback,
/// link-local (cloud metadata), IETF protocol assignments, documentation, benchmarking,
/// multicast, reserved and broadcast.
const BLOCKED_V4: [(Ipv4Addr, u8); 14] = [
    (Ipv4Addr::new(0, 0, 0, 0), 8),
    (Ipv4Addr::new(10, 0, 0, 0), 8),
    (Ipv4Addr::new(100, 64, 0, 0), 10),
    (Ipv4Addr::new(127, 0, 0, 0), 8),
    (Ipv4Addr::new(169, 254, 0, 0), 16),
    (Ipv4Addr::new(172, 16, 0, 0), 12),
    (Ipv4Addr::new(192, 0, 0, 0), 24),
    (Ipv4Addr::new(192, 0, 2, 0), 24),
    (Ipv4Addr::new(192, 168, 0, 0), 16),
    (Ipv4Addr::new(198, 18, 0, 0), 15),
    (Ipv4Addr::new(198, 51, 100, 0), 24),
    (Ipv4Addr::new(203, 0, 113, 0), 24),
    (Ipv4Addr::new(224, 0, 0, 0), 4),
    (Ipv4Addr::new(240, 0, 0, 0), 4),
];

/// IPv6 global unicast is 2000::/3. Everything outside it (loopback, unspecified, v4-compatible,
/// discard, ULA, link-local, site-local, multicast) is refused by not being inside it.
const GLOBAL_UNICAST_V6: (Ipv6Addr, u8) = (Ipv6Addr::new(0x2000, 0, 0, 0, 0, 0, 0, 0), 3);

/// Inside 2000::/3 but never a public endpoint: IETF protocol assignments (Teredo, benchmarking,
/// ORCHID), documentation (both ranges) and 6to4, whose relays tunnel to any IPv4 address.
const BLOCKED_V6: [(Ipv6Addr, u8); 4] = [
    (Ipv6Addr::new(0x2001, 0, 0, 0, 0, 0, 0, 0), 23),
    (Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 0), 32),
    (Ipv6Addr::new(0x2002, 0, 0, 0, 0, 0, 0, 0), 16),
    (Ipv6Addr::new(0x3fff, 0, 0, 0, 0, 0, 0, 0), 20),
];

/// The well-known NAT64 prefix. An agent behind DNS64 sees every IPv4-only site here, so the
/// embedded IPv4 address is vetted instead of the prefix.
const NAT64: (Ipv6Addr, u8) = (Ipv6Addr::new(0x64, 0xff9b, 0, 0, 0, 0, 0, 0), 96);

/// Whether `ip` is a global unicast address a probe may dial. IPv4-mapped IPv6 is judged as the
/// IPv4 address it carries.
#[must_use]
pub fn is_global(ip: IpAddr) -> bool {
    match ip.to_canonical() {
        IpAddr::V4(ip) => !BLOCKED_V4.iter().any(|&block| in_v4(ip, block)),
        IpAddr::V6(ip) => {
            if in_v6(ip, NAT64) {
                let [.., a, b, c, d] = ip.octets();
                return is_global(IpAddr::V4(Ipv4Addr::new(a, b, c, d)));
            }
            in_v6(ip, GLOBAL_UNICAST_V6) && !BLOCKED_V6.iter().any(|&block| in_v6(ip, block))
        }
    }
}

fn in_v4(ip: Ipv4Addr, (net, len): (Ipv4Addr, u8)) -> bool {
    let mask = u32::MAX.checked_shl(32 - u32::from(len)).unwrap_or(0);
    u32::from(ip) & mask == u32::from(net) & mask
}

fn in_v6(ip: Ipv6Addr, (net, len): (Ipv6Addr, u8)) -> bool {
    let mask = u128::MAX.checked_shl(128 - u32::from(len)).unwrap_or(0);
    u128::from(ip) & mask == u128::from(net) & mask
}

#[cfg(test)]
#[path = "probe_tests.rs"]
mod tests;
