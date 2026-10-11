use std::io;
use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use rustls::RootCertStore;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
use tokio::time::{sleep, timeout};
use tokio_rustls::TlsAcceptor;
use uuid::Uuid;

use super::{ProbeConfig, Prober, Resolve, client_config, is_global};
use crate::domain::{
    AgentName, CheckId, Host, HttpCheckError, Job, NotProbed, Observation, Outcome, Phase,
    ProbeKey, ProbeKind, Region, Scheme, Target, UnixMs,
};

const LOOPBACK: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);
const NAME: &str = "probe.test";
/// 2031-01-02T00:00:00Z, the test certificate's not_after.
const NOT_AFTER: i64 = 1_925_078_400;

#[test]
fn only_global_unicast_addresses_are_probeable() {
    let table: &[(&str, bool)] = &[
        // 0/8 and its edge
        ("0.0.0.0", false),
        ("0.255.255.255", false),
        ("1.0.0.0", true),
        // 10/8
        ("10.0.0.0", false),
        ("10.255.255.255", false),
        ("9.255.255.255", true),
        ("11.0.0.0", true),
        // CGNAT 100.64/10
        ("100.64.0.0", false),
        ("100.127.255.255", false),
        ("100.63.255.255", true),
        ("100.128.0.0", true),
        // loopback 127/8
        ("127.0.0.1", false),
        ("127.255.255.255", false),
        ("126.255.255.255", true),
        ("128.0.0.0", true),
        // link-local 169.254/16, cloud metadata included
        ("169.254.169.254", false),
        ("169.254.0.0", false),
        ("169.253.255.255", true),
        ("169.255.0.0", true),
        // 172.16/12
        ("172.16.0.0", false),
        ("172.31.255.255", false),
        ("172.15.255.255", true),
        ("172.32.0.0", true),
        // IETF protocol assignments 192.0.0/24
        ("192.0.0.0", false),
        ("192.0.0.255", false),
        ("191.255.255.255", true),
        ("192.0.1.0", true),
        // documentation 192.0.2/24, 198.51.100/24, 203.0.113/24
        ("192.0.2.1", false),
        ("192.0.3.0", true),
        ("198.51.100.7", false),
        ("198.51.99.255", true),
        ("198.51.101.0", true),
        ("203.0.113.9", false),
        ("203.0.112.255", true),
        ("203.0.114.0", true),
        // 192.168/16
        ("192.168.0.1", false),
        ("192.168.255.255", false),
        ("192.167.255.255", true),
        ("192.169.0.0", true),
        // benchmarking 198.18/15
        ("198.18.0.0", false),
        ("198.19.255.255", false),
        ("198.17.255.255", true),
        ("198.20.0.0", true),
        // multicast 224/4, reserved 240/4, broadcast
        ("224.0.0.1", false),
        ("239.255.255.255", false),
        ("223.255.255.255", true),
        ("240.0.0.1", false),
        ("255.255.255.255", false),
        ("8.8.8.8", true),
        // IPv6 unspecified, loopback, v4-compatible
        ("::", false),
        ("::1", false),
        ("::127.0.0.1", false),
        ("::8.8.8.8", false),
        // IPv4-mapped is judged as its IPv4 address
        ("::ffff:127.0.0.1", false),
        ("::ffff:10.0.0.1", false),
        ("::ffff:169.254.169.254", false),
        ("::ffff:100.64.0.1", false),
        ("::ffff:8.8.8.8", true),
        // discard-only 100::/64
        ("100::1", false),
        // ULA fc00::/7
        ("fc00::1", false),
        ("fdff:ffff::1", false),
        ("fbff:ffff::1", false),
        // link-local fe80::/10, site-local fec0::/10
        ("fe80::1", false),
        ("febf:ffff::1", false),
        ("fec0::1", false),
        // multicast ff00::/8
        ("ff02::1", false),
        ("ff0e::1", false),
        // global unicast 2000::/3 and its edges
        ("2000::1", true),
        ("1fff:ffff::1", false),
        ("3fff:1000::1", true),
        ("4000::1", false),
        ("2606:4700::1111", true),
        // IETF assignments 2001::/23: Teredo, benchmarking, ORCHID
        ("2001::1", false),
        ("2001:0:a00:1::", false),
        ("2001:2::1", false),
        ("2001:1ff:ffff::1", false),
        ("2001:200::1", true),
        // documentation 2001:db8::/32 and 3fff::/20
        ("2001:db8::1", false),
        ("2001:db7:ffff::1", true),
        ("2001:db9::1", true),
        ("3fff::1", false),
        ("3fff:fff:ffff::1", false),
        // 6to4 2002::/16, whatever it embeds
        ("2002:a00:1::1", false),
        ("2002:808:808::1", false),
        ("2001:ffff::1", true),
        ("2003::1", true),
        // NAT64 64:ff9b::/96 is judged by its embedded IPv4 address; local-use NAT64 is refused
        ("64:ff9b::a00:1", false),
        ("64:ff9b::7f00:1", false),
        ("64:ff9b::a9fe:a9fe", false),
        ("64:ff9b::808:808", true),
        ("64:ff9b:1::808:808", false),
    ];
    let wrong: Vec<String> = table
        .iter()
        .filter(|(ip, global)| is_global(ip.parse().unwrap()) != *global)
        .map(|(ip, global)| {
            format!(
                "{ip} should be {}",
                if *global { "allowed" } else { "blocked" }
            )
        })
        .collect();
    assert!(wrong.is_empty(), "{wrong:#?}");
}

#[tokio::test]
async fn a_200_head_returns_the_status_and_sends_the_original_host() {
    let (listener, port) = listener(LOOPBACK).await;
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let head = read_head(&mut stream).await;
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
            .await
            .unwrap();
        head
    });
    let resolver = Answers::new(&["127.0.0.1"]);
    let prober = local(&resolver, ProbeConfig::default(), RootCertStore::empty());
    let seen = observe(&prober, &job(Scheme::Http, named(), port, "/health?x=1")).await;
    assert_eq!(seen.outcome, Outcome::Http { status: 200 });
    assert_eq!(seen.remote_ip, Some(LOOPBACK));
    assert_eq!(resolver.calls(), 1, "resolved exactly once");
    let head = server.await.unwrap().to_ascii_lowercase();
    assert!(head.starts_with("get /health?x=1 http/1.1\r\n"), "{head}");
    assert!(
        head.contains(&format!("\r\nhost: {NAME}:{port}\r\n")),
        "{head}"
    );
    assert!(head.contains("\r\nuser-agent: ployzuptime/1"), "{head}");
    assert!(head.contains("\r\nconnection: close\r\n"), "{head}");
}

#[tokio::test]
async fn a_body_that_never_ends_still_returns_at_the_head() {
    let (listener, port) = listener(LOOPBACK).await;
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        read_head(&mut stream).await;
        let head = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n";
        stream.write_all(head).await.unwrap();
        while stream.write_all(b"1\r\nx\r\n").await.is_ok() {
            sleep(Duration::from_millis(10)).await;
        }
    });
    let resolver = Answers::new(&["127.0.0.1"]);
    let prober = local(&resolver, ProbeConfig::default(), RootCertStore::empty());
    let mut job = job(Scheme::Http, named(), port, "/");
    job.timeout = Duration::from_secs(3);
    let seen = observe(&prober, &job).await;
    assert_eq!(seen.outcome, Outcome::Http { status: 200 });
    assert!(seen.timings.total_ms < 1000, "{:?}", seen.timings);
}

#[tokio::test]
async fn a_redirect_is_recorded_and_not_followed() {
    let (elsewhere, elsewhere_port) = listener(LOOPBACK).await;
    let (listener, port) = listener(LOOPBACK).await;
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        read_head(&mut stream).await;
        let reply = format!(
            "HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:{elsewhere_port}/\r\n\
             Content-Length: 0\r\n\r\n"
        );
        stream.write_all(reply.as_bytes()).await.unwrap();
    });
    let resolver = Answers::new(&["127.0.0.1"]);
    let prober = local(&resolver, ProbeConfig::default(), RootCertStore::empty());
    let seen = observe(&prober, &job(Scheme::Http, named(), port, "/")).await;
    assert_eq!(seen.outcome, Outcome::Http { status: 302 });
    no_connection(&elsewhere).await;
}

#[tokio::test]
async fn a_head_over_the_limit_fails_the_request_phase() {
    let (listener, port) = listener(LOOPBACK).await;
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        read_head(&mut stream).await;
        let big = "a".repeat(20 * 1024);
        let reply = format!("HTTP/1.1 200 OK\r\nX-Big: {big}\r\nContent-Length: 0\r\n\r\n");
        let _ = stream.write_all(reply.as_bytes()).await;
    });
    let resolver = Answers::new(&["127.0.0.1"]);
    let prober = local(&resolver, ProbeConfig::default(), RootCertStore::empty());
    let seen = observe(&prober, &job(Scheme::Http, named(), port, "/")).await;
    let (phase, error, _) = unreachable(&seen);
    assert_eq!((phase, error), (Phase::Request, HttpCheckError::Other));
}

#[tokio::test]
async fn many_small_headers_under_the_byte_limit_still_return_the_status() {
    let (listener, port) = listener(LOOPBACK).await;
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        read_head(&mut stream).await;
        let headers: String = (0..150).map(|i| format!("X-H{i}: v\r\n")).collect();
        let reply = format!("HTTP/1.1 200 OK\r\n{headers}Content-Length: 0\r\n\r\n");
        let _ = stream.write_all(reply.as_bytes()).await;
    });
    let resolver = Answers::new(&["127.0.0.1"]);
    let prober = local(&resolver, ProbeConfig::default(), RootCertStore::empty());
    let seen = observe(&prober, &job(Scheme::Http, named(), port, "/")).await;
    assert_eq!(seen.outcome, Outcome::Http { status: 200 });
}

#[tokio::test]
async fn a_refused_connect_fails_the_connect_phase() {
    let port = closed_port().await;
    let resolver = Answers::new(&["127.0.0.1"]);
    let prober = local(&resolver, ProbeConfig::default(), RootCertStore::empty());
    let seen = observe(&prober, &job(Scheme::Http, named(), port, "/")).await;
    let (phase, error, _) = unreachable(&seen);
    assert_eq!(
        (phase, error),
        (Phase::Connect, HttpCheckError::ConnectionRefused)
    );
    assert_eq!(
        seen.remote_ip,
        Some(LOOPBACK),
        "a started dial still names its address"
    );
}

#[tokio::test]
async fn the_address_index_picks_from_the_sorted_answers() {
    let port = closed_port().await;
    let resolver = Answers::new(&["127.0.0.2", "127.0.0.1"]);
    let prober = local(&resolver, ProbeConfig::default(), RootCertStore::empty());
    let mut job = job(Scheme::Http, named(), port, "/");
    let mut dialled = Vec::new();
    for address in 0..3 {
        job.address = address;
        dialled.push(observe(&prober, &job).await.remote_ip.unwrap().to_string());
    }
    assert_eq!(dialled, ["127.0.0.1", "127.0.0.2", "127.0.0.1"]);
}

#[tokio::test]
async fn an_untrusted_certificate_fails_the_tls_phase() {
    let (listener, port) = listener(LOOPBACK).await;
    let (acceptor, _) = tls_server();
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let _ = acceptor.accept(stream).await;
    });
    let resolver = Answers::new(&["127.0.0.1"]);
    let prober = local(&resolver, ProbeConfig::default(), RootCertStore::empty());
    let seen = observe(&prober, &job(Scheme::Https, named(), port, "/")).await;
    let (phase, error, detail) = unreachable(&seen);
    assert_eq!((phase, error), (Phase::Tls, HttpCheckError::Other));
    assert!(detail.contains("invalid peer certificate"), "{detail}");
    assert_eq!(seen.cert_not_after, None);
}

#[tokio::test]
async fn a_trusted_handshake_sends_sni_and_records_the_leaf_expiry() {
    let (listener, port) = listener(LOOPBACK).await;
    let (acceptor, root) = tls_server();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut tls = acceptor.accept(stream).await.unwrap();
        let sni = tls.get_ref().1.server_name().map(str::to_owned);
        let head = read_head(&mut tls).await;
        tls.write_all(b"HTTP/1.1 204 No Content\r\n\r\n")
            .await
            .unwrap();
        tls.flush().await.unwrap();
        (sni, head)
    });
    let mut roots = RootCertStore::empty();
    roots.add(root).unwrap();
    let resolver = Answers::new(&["127.0.0.1"]);
    let prober = local(&resolver, ProbeConfig::default(), roots);
    let seen = observe(&prober, &job(Scheme::Https, named(), port, "/")).await;
    assert_eq!(seen.outcome, Outcome::Http { status: 204 });
    assert_eq!(seen.cert_not_after, Some(NOT_AFTER));
    let (sni, head) = server.await.unwrap();
    assert_eq!(sni.as_deref(), Some(NAME));
    assert!(
        head.to_ascii_lowercase()
            .contains(&format!("\r\nhost: {NAME}:{port}\r\n"))
    );
    assert!(seen.timings.tls_ms <= seen.timings.total_ms);
}

#[tokio::test]
async fn a_server_that_stalls_after_accept_times_out_in_the_request_phase() {
    let (listener, port) = listener(LOOPBACK).await;
    let _server = hold_connections(listener);
    let resolver = Answers::new(&["127.0.0.1"]);
    let prober = local(&resolver, ProbeConfig::default(), RootCertStore::empty());
    let mut job = job(Scheme::Http, named(), port, "/");
    job.timeout = Duration::from_millis(400);
    let seen = observe(&prober, &job).await;
    let (phase, error, _) = unreachable(&seen);
    assert_eq!((phase, error), (Phase::Request, HttpCheckError::TimedOut));
    assert!(
        (300..1000).contains(&seen.timings.total_ms),
        "{:?}",
        seen.timings
    );
}

#[tokio::test]
async fn a_server_that_stalls_the_handshake_times_out_in_the_tls_phase() {
    let (listener, port) = listener(LOOPBACK).await;
    let _server = hold_connections(listener);
    let resolver = Answers::new(&["127.0.0.1"]);
    let config = ProbeConfig {
        tls_timeout: Duration::from_millis(200),
        ..ProbeConfig::default()
    };
    let prober = local(&resolver, config, RootCertStore::empty());
    let seen = observe(&prober, &job(Scheme::Https, named(), port, "/")).await;
    let (phase, error, detail) = unreachable(&seen);
    assert_eq!((phase, error), (Phase::Tls, HttpCheckError::TimedOut));
    assert_eq!(detail, "tls timed out after 200 ms");
    assert!(seen.timings.total_ms < 1000, "{:?}", seen.timings);
}

#[tokio::test]
async fn a_silent_resolver_times_out_in_the_dns_phase() {
    let config = ProbeConfig {
        dns_timeout: Duration::from_millis(100),
        ..ProbeConfig::default()
    };
    let prober = local(Silent, config, RootCertStore::empty());
    let seen = observe(&prober, &job(Scheme::Http, named(), 80, "/")).await;
    let (phase, error, detail) = unreachable(&seen);
    assert_eq!((phase, error), (Phase::Dns, HttpCheckError::TimedOut));
    assert_eq!(detail, "dns timed out after 100 ms");
    assert_eq!(seen.remote_ip, None);
}

#[tokio::test]
async fn a_phase_never_outlives_the_job_timeout() {
    let prober = local(Silent, ProbeConfig::default(), RootCertStore::empty());
    let mut job = job(Scheme::Http, named(), 80, "/");
    job.timeout = Duration::from_millis(150);
    let seen = observe(&prober, &job).await;
    let (phase, error, _) = unreachable(&seen);
    assert_eq!((phase, error), (Phase::Dns, HttpCheckError::TimedOut));
    assert!(seen.timings.total_ms < 1000, "{:?}", seen.timings);
}

#[tokio::test]
async fn a_name_with_no_answers_fails_the_dns_phase() {
    let resolver = Answers::new(&[]);
    let prober = local(&resolver, ProbeConfig::default(), RootCertStore::empty());
    let seen = observe(&prober, &job(Scheme::Http, named(), 80, "/")).await;
    assert_eq!(
        unreachable(&seen),
        (Phase::Dns, HttpCheckError::Other, "no addresses")
    );
}

#[tokio::test]
async fn a_name_resolving_to_loopback_is_not_probed() {
    let (listener, port) = listener(LOOPBACK).await;
    let resolver = Answers::new(&["127.0.0.1"]);
    let mut prober = local(&resolver, ProbeConfig::default(), RootCertStore::empty());
    prober.loopback_ok = false;
    let seen = observe(&prober, &job(Scheme::Http, named(), port, "/")).await;
    assert_eq!(seen.outcome, Outcome::NotProbed(NotProbed::PrivateAddress));
    assert_eq!(seen.remote_ip, None);
    no_connection(&listener).await;
}

#[tokio::test]
async fn one_private_answer_refuses_the_whole_target() {
    let (listener, port) = listener(LOOPBACK).await;
    // Sorted, index 1 is 127.0.0.1, which loopback_ok would let through on its own.
    let resolver = Answers::new(&["127.0.0.1", "10.0.0.1"]);
    let prober = local(&resolver, ProbeConfig::default(), RootCertStore::empty());
    let mut job = job(Scheme::Http, named(), port, "/");
    job.address = 1;
    let seen = observe(&prober, &job).await;
    assert_eq!(seen.outcome, Outcome::NotProbed(NotProbed::PrivateAddress));
    no_connection(&listener).await;
}

#[tokio::test]
async fn an_ip_literal_is_vetted_without_resolving() {
    let resolver = Answers::new(&["8.8.8.8"]);
    let mut prober = local(&resolver, ProbeConfig::default(), RootCertStore::empty());
    prober.loopback_ok = false;
    let seen = observe(&prober, &job(Scheme::Http, Host::Ip(LOOPBACK), 80, "/")).await;
    assert_eq!(seen.outcome, Outcome::NotProbed(NotProbed::PrivateAddress));
    assert_eq!(resolver.calls(), 0);
}

#[tokio::test]
async fn ipv6_answers_are_not_dialled_unless_allowed() {
    let (listener, port) = listener(LOOPBACK).await;
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        read_head(&mut stream).await;
        stream.write_all(b"HTTP/1.1 200 OK\r\n\r\n").await.unwrap();
    });
    let dual = Answers::new(&["127.0.0.1", "2606:4700::1111"]);
    let prober = local(&dual, ProbeConfig::default(), RootCertStore::empty());
    let mut job = job(Scheme::Http, named(), port, "/");
    job.address = 1;
    let seen = observe(&prober, &job).await;
    assert_eq!(seen.outcome, Outcome::Http { status: 200 });

    let only_v6 = Answers::new(&["2606:4700::1111"]);
    let prober = local(&only_v6, ProbeConfig::default(), RootCertStore::empty());
    let seen = observe(&prober, &job).await;
    assert_eq!(seen.outcome, Outcome::NotProbed(NotProbed::PrivateAddress));
    assert_eq!(seen.remote_ip, None);
}

#[tokio::test]
async fn the_per_address_cap_declines_and_releases() {
    let (listener, port) = listener(LOOPBACK).await;
    let _server = hold_connections(listener);
    let resolver = Answers::new(&["127.0.0.1"]);
    let config = ProbeConfig {
        per_address: 1,
        ..ProbeConfig::default()
    };
    let prober = local(&resolver, config, RootCertStore::empty());
    let mut job = job(Scheme::Http, named(), port, "/");
    job.timeout = Duration::from_millis(300);
    let (first, second) = tokio::join!(observe(&prober, &job), async {
        sleep(Duration::from_millis(100)).await;
        observe(&prober, &job).await
    });
    assert_eq!(unreachable(&first).0, Phase::Request);
    assert_eq!(second.outcome, Outcome::NotProbed(NotProbed::Saturated));
    let third = observe(&prober, &job).await;
    assert_eq!(
        unreachable(&third).0,
        Phase::Request,
        "the first probe's claim was released"
    );
}

#[tokio::test]
async fn the_global_cap_declines_across_addresses() {
    let (listener, port) = listener(LOOPBACK).await;
    let _server = hold_connections(listener);
    let first_answers = Answers::new(&["127.0.0.1"]);
    let config = ProbeConfig {
        max_inflight: 1,
        ..ProbeConfig::default()
    };
    let prober = local(&first_answers, config, RootCertStore::empty());
    let mut job = job(Scheme::Http, named(), port, "/");
    job.timeout = Duration::from_millis(300);
    let other = Job {
        target: Arc::new(Target {
            host: Host::Ip(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 2))),
            ..(*job.target).clone()
        }),
        ..job.clone()
    };
    let (first, second) = tokio::join!(observe(&prober, &job), async {
        sleep(Duration::from_millis(100)).await;
        observe(&prober, &other).await
    });
    assert_eq!(unreachable(&first).0, Phase::Request);
    assert_eq!(second.outcome, Outcome::NotProbed(NotProbed::Saturated));
}

struct Answers {
    ips: Vec<IpAddr>,
    calls: AtomicUsize,
}

impl Answers {
    fn new(ips: &[&str]) -> Answers {
        Answers {
            ips: ips.iter().map(|ip| ip.parse().unwrap()).collect(),
            calls: AtomicUsize::new(0),
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl Resolve for &Answers {
    async fn resolve(&self, _host: &str) -> io::Result<Vec<IpAddr>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(self.ips.clone())
    }
}

struct Silent;

impl Resolve for Silent {
    async fn resolve(&self, _host: &str) -> io::Result<Vec<IpAddr>> {
        std::future::pending().await
    }
}

/// A prober allowed to dial loopback, so it can reach the tests' servers.
fn local<R: Resolve>(resolver: R, config: ProbeConfig, roots: RootCertStore) -> Prober<R> {
    let crypto = Arc::new(rustls::crypto::ring::default_provider());
    let tls = client_config(crypto, roots).unwrap();
    let mut prober = Prober::with(resolver, tls, config, AgentName("test".into()));
    prober.loopback_ok = true;
    prober
}

fn named() -> Host {
    Host::Name(NAME.to_owned())
}

fn job(scheme: Scheme, host: Host, port: u16, path: &str) -> Job {
    Job {
        key: ProbeKey {
            check: CheckId(Uuid::nil()),
            region: Region::parse("us-west2").unwrap(),
            scheduled_at: UnixMs(0),
            kind: ProbeKind::Routine,
        },
        target: Arc::new(Target {
            scheme,
            host,
            port,
            path_and_query: path.to_owned(),
        }),
        timeout: Duration::from_secs(5),
        address: 0,
    }
}

async fn observe<R: Resolve>(prober: &Prober<R>, job: &Job) -> Observation {
    timeout(Duration::from_secs(10), prober.probe(job, UnixMs(0)))
        .await
        .expect("the probe kept to its timeout")
}

fn unreachable(seen: &Observation) -> (Phase, HttpCheckError, &str) {
    let Outcome::Unreachable {
        phase,
        error,
        detail,
    } = &seen.outcome
    else {
        panic!("expected Unreachable, got {:?}", seen.outcome);
    };
    (*phase, *error, detail.as_str())
}

async fn listener(ip: IpAddr) -> (TcpListener, u16) {
    let listener = TcpListener::bind((ip, 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    (listener, port)
}

async fn closed_port() -> u16 {
    let (listener, port) = listener(LOOPBACK).await;
    drop(listener);
    port
}

async fn no_connection(listener: &TcpListener) {
    let accepted = timeout(Duration::from_millis(200), listener.accept()).await;
    assert!(
        accepted.is_err(),
        "nothing should have dialled this listener"
    );
}

/// Accepts every connection and never answers.
fn hold_connections(listener: TcpListener) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut held: Vec<TcpStream> = Vec::new();
        while let Ok((stream, _)) = listener.accept().await {
            held.push(stream);
        }
    })
}

async fn read_head<S: AsyncRead + Unpin>(stream: &mut S) -> String {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if stream.read(&mut byte).await.unwrap() == 0 {
            break;
        }
        head.extend_from_slice(&byte);
    }
    String::from_utf8(head).unwrap()
}

/// A TLS server for `NAME` with a self-signed certificate, and that certificate to trust.
fn tls_server() -> (TlsAcceptor, CertificateDer<'static>) {
    let mut params = rcgen::CertificateParams::new(vec![NAME.to_owned()]).unwrap();
    params.not_after = rcgen::date_time_ymd(2031, 1, 2);
    let key = rcgen::KeyPair::generate().unwrap();
    let cert = params.self_signed(&key).unwrap().der().clone();
    let crypto = Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ServerConfig::builder_with_provider(crypto)
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![cert.clone()],
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der())),
        )
        .unwrap();
    (TlsAcceptor::from(Arc::new(config)), cert)
}
