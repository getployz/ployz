use std::{
    cell::Cell,
    net::{TcpListener as StdTcpListener, UdpSocket as StdUdpSocket},
};

use tokio::{
    io::AsyncReadExt as _,
    net::{TcpStream, UdpSocket},
};

use super::super::wire::{self, Answer};
use super::*;

const KEY: &str = "abababababababababababababababababababababababababababababababab";
const NONCE: &str = "n0nce-n0nce-n0nce-n0nc";
fn key() -> CheckinKey {
    CheckinKey::new(iroh::SecretKey::from_bytes(&[7; 32]))
}

const PATIENCE: Duration = Duration::from_secs(3);

thread_local! {
    /// Stands in for `net.ipv6.bindv6only=1`, which makes every IPv6 socket IPv6-only by default.
    pub(super) static ONLY_V6_BY_DEFAULT: Cell<bool> = const { Cell::new(false) };
    /// Fails every IPv6 bind with this errno.
    pub(super) static IPV6_FAILS_WITH: Cell<Option<i32>> = const { Cell::new(None) };
    /// Gives compressed test windows a bind budget without waiting for D8's first attempt.
    pub(in crate::checkin) static LEAD: Cell<Option<Duration>> = const { Cell::new(None) };
    /// Delays every bind by this long, the way a loaded host delays `open`.
    pub(super) static BIND_STALL: Cell<Duration> = const { Cell::new(Duration::ZERO) };
    pub(super) static STALL_PORT: Cell<Option<Port>> = const { Cell::new(None) };
}

pub(super) struct BindFaults {
    only_v6: bool,
    ipv6_error: Option<i32>,
    stall: Duration,
    port: Option<Port>,
}

impl BindFaults {
    pub(super) fn capture() -> Self {
        Self {
            only_v6: ONLY_V6_BY_DEFAULT.get(),
            ipv6_error: IPV6_FAILS_WITH.get(),
            stall: BIND_STALL.get(),
            port: STALL_PORT.get(),
        }
    }

    pub(super) fn enter(self) -> Self {
        Self {
            only_v6: ONLY_V6_BY_DEFAULT.replace(self.only_v6),
            ipv6_error: IPV6_FAILS_WITH.replace(self.ipv6_error),
            stall: BIND_STALL.replace(self.stall),
            port: STALL_PORT.replace(self.port),
        }
    }
}

impl Drop for BindFaults {
    fn drop(&mut self) {
        ONLY_V6_BY_DEFAULT.set(self.only_v6);
        IPV6_FAILS_WITH.set(self.ipv6_error);
        BIND_STALL.set(self.stall);
        STALL_PORT.set(self.port);
    }
}

/// Held to stall every save, the way a slow disk stalls `sync_all`. It is process-wide, so a test
/// holding it stalls saves in every test sharing the process; nextest runs each test alone.
pub(in crate::checkin) static SAVES: Mutex<()> = Mutex::new(());

fn free_port() -> u16 {
    loop {
        let port = StdTcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        if bindable(port) {
            return port;
        }
    }
}

fn handout(handout: &str, window_ms: u64, listen: &[Port], send: &[u16]) -> Handout {
    if window_ms <= 2_000 && LEAD.get().is_none() {
        LEAD.set(Some(Duration::from_millis(window_ms * 3 / 10)));
    }
    Handout {
        nonce: NONCE.to_owned(),
        handout: handout.to_owned(),
        window_ms,
        listen: listen.to_vec(),
        send: send
            .iter()
            .map(|&port| Target {
                target: IpAddr::V4(Ipv4Addr::LOCALHOST),
                port,
            })
            .collect(),
    }
}

async fn eventually(mut check: impl FnMut() -> bool) {
    let deadline = Instant::now() + PATIENCE;
    while !check() {
        assert!(Instant::now() < deadline, "condition never held");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// A family the host lacks counts as free, so an IPv4-only host still finds a port.
fn bindable(port: u16) -> bool {
    let free = |bound: io::Result<()>| {
        bound.err().map(|error| error.kind()) != Some(io::ErrorKind::AddrInUse)
    };
    ["0.0.0.0", "::"].into_iter().all(|any| {
        free(StdTcpListener::bind((any, port)).map(drop))
            && free(StdUdpSocket::bind((any, port)).map(drop))
    })
}

async fn greeting_from(port: u16) -> Vec<u8> {
    let deadline = Instant::now() + PATIENCE;
    let mut stream = loop {
        if let Ok(stream) = TcpStream::connect(("127.0.0.1", port)).await {
            break stream;
        }
        assert!(Instant::now() < deadline, "nothing listened on {port}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    let mut read = Vec::new();
    stream.read_to_end(&mut read).await.unwrap();
    read
}

#[test]
fn greets_with_the_proof_cloud_computes() {
    assert_eq!(greeting("h4ndout", KEY, 80), "0WewX55aVeyQV0-6rurfS2");
    assert_eq!(greeting("h4ndout", KEY, 443), "7LzmdbdyFuBO_n_iGzlMIZ");
}

#[tokio::test]
async fn each_accept_reads_its_own_port_greeting_and_never_the_nonce() {
    let port = free_port();
    let mut probes = Probes::default();
    probes
        .open(
            handout("h4ndout", 5_000, &[Port::Tcp(port)], &[]),
            &key(),
            Instant::now(),
        )
        .await;

    for _ in 0..2 {
        let read = greeting_from(port).await;
        assert_eq!(
            read,
            greeting("h4ndout", key().public_hex(), port).into_bytes()
        );
        assert!(!String::from_utf8_lossy(&read).contains(NONCE));
    }
}

#[tokio::test(start_paused = true)]
async fn a_longer_window_is_clamped_to_ten_seconds() {
    let port = free_port();
    let opened = Instant::now();
    let mut probes = Probes::default();
    probes
        .open(
            handout("long", 60_000, &[Port::Udp(port)], &[]),
            &key(),
            Instant::now(),
        )
        .await;

    probes.finish().await;

    assert_eq!(opened.elapsed(), WINDOW_CAP);
    assert!(bindable(port));
}

#[tokio::test]
async fn every_socket_closes_when_the_window_ends_and_the_report_stays() {
    let (tcp, udp) = (free_port(), free_port());
    let mut probes = Probes::default();
    probes
        .open(
            handout("short", 300, &[Port::Tcp(tcp), Port::Udp(udp)], &[]),
            &key(),
            Instant::now(),
        )
        .await;
    greeting_from(tcp).await;

    eventually(|| bindable(tcp) && bindable(udp)).await;
    assert_eq!(probes.report().await.unwrap().handout, "short");
}

#[tokio::test]
async fn close_frees_every_port_before_it_returns() {
    let (tcp, udp) = (free_port(), free_port());
    let mut probes = Probes::default();
    probes
        .open(
            handout("closing", 10_000, &[Port::Tcp(tcp), Port::Udp(udp)], &[]),
            &key(),
            Instant::now(),
        )
        .await;
    greeting_from(tcp).await;

    probes.close().await;

    assert!(bindable(tcp) && bindable(udp));
}

#[tokio::test]
async fn a_port_another_process_holds_goes_to_bind_failed() {
    let (held_tcp, held_udp, free) = (free_port(), free_port(), free_port());
    let _tcp = StdTcpListener::bind(("0.0.0.0", held_tcp)).unwrap();
    let _udp = StdUdpSocket::bind(("0.0.0.0", held_udp)).unwrap();
    let mut probes = Probes::default();
    probes
        .open(
            handout(
                "busy",
                1_000,
                &[Port::Tcp(held_tcp), Port::Udp(held_udp), Port::Tcp(free)],
                &[],
            ),
            &key(),
            Instant::now(),
        )
        .await;

    greeting_from(free).await;
    probes.finish().await;
    assert_eq!(
        probes.report().await.unwrap().bind_failed,
        [format!("tcp:{held_tcp}"), format!("udp:{held_udp}")]
    );
}

#[tokio::test]
async fn peers_hear_each_other_once_by_address() {
    let (a, b) = (free_port(), free_port());
    let (mut first, mut second) = (Probes::default(), Probes::default());
    first
        .open(
            handout("a", 1_500, &[Port::Udp(a)], &[b]),
            &key(),
            Instant::now(),
        )
        .await;
    second
        .open(
            handout("b", 1_500, &[Port::Udp(b)], &[a]),
            &key(),
            Instant::now(),
        )
        .await;

    tokio::join!(first.finish(), second.finish());
    for probes in [&mut first, &mut second] {
        assert_eq!(probes.report().await.unwrap().heard_from, ["127.0.0.1"]);
    }
}

#[tokio::test]
async fn only_the_exact_nonce_counts_as_heard() {
    let port = free_port();
    let mut probes = Probes::default();
    probes
        .open(
            handout("nonce", 1_000, &[Port::Udp(port)], &[]),
            &key(),
            Instant::now(),
        )
        .await;
    let wrong = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let target = ("127.0.0.1", port);

    for payload in [
        b"some-other-nonce-value".as_slice(),
        format!("{NONCE}x").as_bytes(),
        NONCE.get(..21).unwrap().as_bytes(),
    ] {
        wrong.send_to(payload, target).await.unwrap();
    }
    let right = UdpSocket::bind("[::1]:0").await.unwrap();
    right
        .send_to(NONCE.as_bytes(), ("::1", port))
        .await
        .unwrap();

    probes.finish().await;
    assert_eq!(probes.report().await.unwrap().heard_from, ["::1"]);
}

#[tokio::test]
async fn a_new_handout_replaces_the_window_even_with_the_same_nonce() {
    let port = free_port();
    let mut probes = Probes::default();
    probes
        .open(
            handout("first", 10_000, &[Port::Tcp(port)], &[]),
            &key(),
            Instant::now(),
        )
        .await;
    greeting_from(port).await;

    probes
        .open(
            handout("second", 1_000, &[Port::Tcp(port)], &[]),
            &key(),
            Instant::now(),
        )
        .await;

    assert_eq!(
        greeting_from(port).await,
        greeting("second", key().public_hex(), port).into_bytes()
    );
    probes.finish().await;
    let report = probes.report().await.unwrap();
    assert_eq!(report.handout, "second");
    assert!(report.bind_failed.is_empty());
}

#[tokio::test]
async fn an_ipv6_only_default_still_listens_and_reports_busy_ports_over_ipv4() {
    ONLY_V6_BY_DEFAULT.set(true);
    let (tcp, udp, held_tcp, held_udp) = (free_port(), free_port(), free_port(), free_port());
    let _tcp = StdTcpListener::bind(("0.0.0.0", held_tcp)).unwrap();
    let _udp = StdUdpSocket::bind(("0.0.0.0", held_udp)).unwrap();
    let mut probes = Probes::default();
    probes
        .open(
            handout(
                "v6only",
                1_500,
                &[
                    Port::Tcp(tcp),
                    Port::Udp(udp),
                    Port::Tcp(held_tcp),
                    Port::Udp(held_udp),
                ],
                &[],
            ),
            &key(),
            Instant::now(),
        )
        .await;

    assert_eq!(
        greeting_from(tcp).await,
        greeting("v6only", key().public_hex(), tcp).into_bytes()
    );
    let peer = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    peer.send_to(NONCE.as_bytes(), ("127.0.0.1", udp))
        .await
        .unwrap();
    probes.finish().await;
    let report = probes.report().await.unwrap();
    assert_eq!(report.heard_from, ["127.0.0.1"]);
    assert_eq!(
        report.bind_failed,
        [format!("tcp:{held_tcp}"), format!("udp:{held_udp}")]
    );
}

#[tokio::test]
async fn only_a_host_without_ipv6_falls_back_to_ipv4() {
    let (refused, unsupported) = (free_port(), free_port());
    let mut probes = Probes::default();

    IPV6_FAILS_WITH.set(Some(Errno::ACCESS.raw_os_error()));
    probes
        .open(
            handout("denied", 1_000, &[Port::Tcp(refused)], &[]),
            &key(),
            Instant::now(),
        )
        .await;
    assert!(TcpStream::connect(("127.0.0.1", refused)).await.is_err());
    probes.finish().await;
    assert_eq!(
        probes.report().await.unwrap().bind_failed,
        [format!("tcp:{refused}")]
    );

    IPV6_FAILS_WITH.set(Some(Errno::AFNOSUPPORT.raw_os_error()));
    probes
        .open(
            handout("v4", 1_000, &[Port::Tcp(unsupported)], &[]),
            &key(),
            Instant::now(),
        )
        .await;
    assert_eq!(
        greeting_from(unsupported).await,
        greeting("v4", key().public_hex(), unsupported).into_bytes()
    );
    probes.finish().await;
    assert!(probes.report().await.unwrap().bind_failed.is_empty());
}

#[tokio::test]
async fn a_window_still_open_reports_nothing_and_close_forgets_it() {
    let port = free_port();
    let mut probes = Probes::default();
    probes
        .open(
            handout("open", 10_000, &[Port::Udp(port)], &[]),
            &key(),
            Instant::now(),
        )
        .await;
    let peer = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    peer.send_to(NONCE.as_bytes(), ("127.0.0.1", port))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;

    assert_eq!(probes.report().await, None);
    probes.close().await;
    assert_eq!(probes.report().await, None);
}

#[tokio::test]
async fn a_cancelled_finish_leaves_no_window_holding_the_ports() {
    let port = free_port();
    let mut probes = Probes::default();
    probes
        .open(
            handout("cancelled", 10_000, &[Port::Udp(port)], &[]),
            &key(),
            Instant::now(),
        )
        .await;

    let waited = tokio::time::timeout(Duration::from_millis(50), probes.finish()).await;
    assert!(waited.is_err());
    drop(probes);

    eventually(|| bindable(port)).await;
}

#[tokio::test]
async fn a_window_that_runs_out_is_saved_for_the_next_process() {
    let dir = tempfile::tempdir().unwrap();
    let saved = dir.path().join("probe.json");
    let port = free_port();
    let mut probes = Probes::load(saved.clone());
    probes
        .open(
            handout("short", 300, &[Port::Udp(port)], &[]),
            &key(),
            Instant::now(),
        )
        .await;

    probes.finish().await;

    assert_eq!(Probes::load(saved).report().await, probes.report().await);
    assert_eq!(probes.report().await.unwrap().handout, "short");
}

#[tokio::test]
async fn a_slow_save_holds_no_port_and_no_check_in() {
    let dir = tempfile::tempdir().unwrap();
    let saved = dir.path().join("probe.json");
    let (tcp, udp) = (free_port(), free_port());
    let (held, holding) = std::sync::mpsc::channel();
    let (release, released) = std::sync::mpsc::channel::<()>();
    let disk = std::thread::spawn(move || {
        let _stalled = SAVES.lock().unwrap();
        held.send(()).unwrap();
        let _ = released.recv_timeout(Duration::from_secs(3));
    });
    holding.recv().unwrap();
    let opened = Instant::now();
    let mut probes = Probes::load(saved.clone());
    probes
        .open(
            handout("slow", 200, &[Port::Tcp(tcp), Port::Udp(udp)], &[]),
            &key(),
            Instant::now(),
        )
        .await;

    tokio::time::sleep(Duration::from_millis(500)).await;

    assert!(
        opened.elapsed() < Duration::from_millis(1_500),
        "the runtime stalled"
    );
    assert!(TcpStream::connect(("127.0.0.1", tcp)).await.is_err());
    assert!(bindable(tcp) && bindable(udp));
    assert!(!saved.exists());
    let report = tokio::time::timeout(Duration::from_millis(500), probes.report())
        .await
        .expect("the report waits on no save");
    assert_eq!(report.unwrap().handout, "slow");
    release.send(()).unwrap();
    disk.join().unwrap();
    probes.finish().await;
    assert_eq!(Probes::load(saved).report().await.unwrap().handout, "slow");
}

/// Stalls every save until the returned sender drops or `seconds` pass.
fn stall_saves(seconds: u64) -> (std::sync::mpsc::Sender<()>, std::thread::JoinHandle<()>) {
    let (held, holding) = std::sync::mpsc::channel();
    let (release, released) = std::sync::mpsc::channel::<()>();
    let disk = std::thread::spawn(move || {
        let _stalled = SAVES.lock().unwrap();
        held.send(()).unwrap();
        let _ = released.recv_timeout(Duration::from_secs(seconds));
    });
    holding.recv().unwrap();
    (release, disk)
}

#[tokio::test]
async fn a_replacement_listens_at_once_while_the_old_window_is_still_saving() {
    let dir = tempfile::tempdir().unwrap();
    let saved = dir.path().join("probe.json");
    let (old, new) = (free_port(), free_port());
    let (release, disk) = stall_saves(5);
    let mut probes = Probes::load(saved.clone());
    probes
        .open(
            handout("old", 200, &[Port::Tcp(old)], &[]),
            &key(),
            Instant::now(),
        )
        .await;
    tokio::time::sleep(Duration::from_millis(400)).await;

    let handed = Instant::now();
    probes
        .open(
            handout("new", 400, &[Port::Tcp(new)], &[]),
            &key(),
            Instant::now(),
        )
        .await;

    assert!(
        handed.elapsed() < Duration::from_millis(200),
        "{:?}",
        handed.elapsed()
    );
    assert_eq!(
        greeting_from(new).await,
        greeting("new", key().public_hex(), new).into_bytes()
    );
    tokio::time::sleep(Duration::from_millis(600)).await;
    let report = probes.report().await.unwrap();
    assert_eq!(report.handout, "new");
    assert!(report.bind_failed.is_empty());
    drop(release);
    disk.join().unwrap();
    probes.finish().await;
    assert_eq!(Probes::load(saved).report().await.unwrap().handout, "new");
}

#[test]
fn a_port_must_listen_three_seconds_before_the_window_ends() {
    assert_eq!(bind_deadline(WINDOW_CAP), Duration::from_secs(7));
}

#[tokio::test]
async fn a_later_slow_bind_does_not_extend_an_earlier_listener() {
    let (udp, first, second) = (free_port(), free_port(), free_port());
    STALL_PORT.set(Some(Port::Tcp(second)));
    BIND_STALL.set(Duration::from_millis(1_600));
    LEAD.set(Some(Duration::from_millis(300)));
    let started = std::time::Instant::now();
    let observer = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        assert!(std::net::TcpStream::connect(("127.0.0.1", first)).is_ok());
        assert!(!bindable(udp));
        std::thread::sleep(
            (started + Duration::from_millis(1_100))
                .saturating_duration_since(std::time::Instant::now()),
        );
        assert!(
            std::net::TcpStream::connect(("127.0.0.1", first)).is_err(),
            "the earlier listener outlived its window"
        );
        assert!(
            bindable(first) && bindable(udp),
            "ports were not released at expiry"
        );
    });
    let mut probes = Probes::default();
    probes
        .open(
            handout(
                "later-bind",
                1_000,
                &[Port::Udp(udp), Port::Tcp(first), Port::Tcp(second)],
                &[],
            ),
            &key(),
            Instant::now(),
        )
        .await;
    probes.finish().await;
    let elapsed = started.elapsed();
    observer.join().unwrap();
    assert!(
        elapsed < Duration::from_millis(1_200),
        "report finished after {elapsed:?}"
    );
    assert!(bindable(udp) && bindable(first) && bindable(second));
    assert_eq!(
        probes.report().await.unwrap().bind_failed,
        [format!("tcp:{second}")]
    );
}

#[tokio::test]
async fn shorter_windows_still_greet_cloud_on_a_free_port() {
    for millis in [2_500, 3_000] {
        let port = free_port();
        let mut probes = Probes::default();
        probes
            .open(
                handout("shorter", millis, &[Port::Tcp(port)], &[]),
                &key(),
                Instant::now(),
            )
            .await;
        assert_eq!(
            greeting_from(port).await,
            greeting("shorter", key().public_hex(), port).into_bytes()
        );
        probes.close().await;
    }
}

#[tokio::test]
async fn a_bind_that_finishes_after_the_deadline_reports_as_failed_and_stays_refused() {
    let dir = tempfile::tempdir().unwrap();
    let tcp = free_port();
    LEAD.set(Some(Duration::from_millis(700)));
    BIND_STALL.set(Duration::from_millis(500));
    let mut probes = Probes::load(dir.path().join("probe.json"));

    probes
        .open(
            handout("late", 1_000, &[Port::Tcp(tcp)], &[]),
            &key(),
            Instant::now(),
        )
        .await;

    assert!(TcpStream::connect(("127.0.0.1", tcp)).await.is_err());
    assert!(bindable(tcp));
    probes.finish().await;
    assert_eq!(
        probes.report().await.unwrap().bind_failed,
        [format!("tcp:{tcp}")]
    );
}

#[tokio::test]
async fn a_bind_after_d8_s_only_attempt_reports_failed_in_a_short_window() {
    let dir = tempfile::tempdir().unwrap();
    let tcp = free_port();
    BIND_STALL.set(Duration::from_millis(1_200));
    let mut probes = Probes::load(dir.path().join("probe.json"));
    let sent = std::time::Instant::now();
    let observer = std::thread::spawn(move || {
        std::thread::sleep(
            (sent + Duration::from_secs(1)).saturating_duration_since(std::time::Instant::now()),
        );
        assert!(std::net::TcpStream::connect(("127.0.0.1", tcp)).is_err());
    });

    probes
        .open(
            handout("slow", 2_500, &[Port::Tcp(tcp)], &[]),
            &key(),
            Instant::now(),
        )
        .await;

    probes.finish().await;
    observer.join().unwrap();
    assert_eq!(
        probes.report().await.unwrap().bind_failed,
        [format!("tcp:{tcp}")]
    );
    assert!(bindable(tcp));
}

#[tokio::test]
async fn windows_without_a_usable_d8_attempt_report_every_port_failed() {
    for millis in [1, 500, 1_000, 1_500, 2_000] {
        let (tcp, udp) = (free_port(), free_port());
        let mut probes = Probes::default();
        let handout = handout("no-attempt", millis, &[Port::Tcp(tcp), Port::Udp(udp)], &[]);
        LEAD.set(None);
        probes.open(handout, &key(), Instant::now()).await;
        assert!(bindable(tcp) && bindable(udp));
        probes.finish().await;
        assert_eq!(
            probes.report().await.unwrap().bind_failed,
            [format!("tcp:{tcp}"), format!("udp:{udp}")]
        );
    }
}

#[tokio::test]
async fn a_dropped_owner_s_save_never_lands_over_a_newer_window() {
    let dir = tempfile::tempdir().unwrap();
    let saved = dir.path().join("probe.json");
    let port = free_port();
    let (release, disk) = stall_saves(5);
    let mut dropped = Probes::load(saved.clone());
    dropped
        .open(
            handout("old", 200, &[Port::Udp(port)], &[]),
            &key(),
            Instant::now(),
        )
        .await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    drop(dropped);

    let mut probes = Probes::load(saved.clone());
    probes
        .open(
            handout("new", 10_000, &[Port::Udp(port)], &[]),
            &key(),
            Instant::now(),
        )
        .await;
    probes.close().await;
    drop(release);
    disk.join().unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    drop(SAVES.lock().unwrap());

    assert!(!saved.exists());
    assert_eq!(Probes::load(saved).report().await, None);
}

#[test]
fn a_wait_carries_the_handout_and_an_unreadable_one_leaves_the_wait() {
    let body = br#"{"kind":"wait","retryAfter":10,"probe":{"nonce":"n0nce-n0nce-n0nce-n0nc","handout":"h","windowMs":10000,"listen":["udp:51820","tcp:80","tcp:443"],"send":[{"target":"203.0.113.7","port":51820}]}}"#;
    let Some(Answer::Probe(after, handout)) = wire::answer(body) else {
        panic!("expected a probe");
    };
    assert_eq!(after, Duration::from_secs(10));
    assert_eq!(
        handout.listen,
        [Port::Udp(51820), Port::Tcp(80), Port::Tcp(443)]
    );

    let unreadable =
        br#"{"kind":"wait","retryAfter":10,"probe":{"nonce":"n","listen":["sctp:1"]}}"#;
    assert_eq!(
        wire::answer(unreadable),
        Some(Answer::Wait(Duration::from_secs(10)))
    );
}

#[test]
fn the_report_goes_out_in_cloud_field_names() {
    let report = Report {
        handout: "h".to_owned(),
        heard_from: vec!["198.51.100.4".to_owned()],
        bind_failed: vec!["tcp:80".to_owned()],
    };
    assert_eq!(
        serde_json::to_value(report).unwrap(),
        serde_json::json!({"handout": "h", "heardFrom": ["198.51.100.4"], "bindFailed": ["tcp:80"]})
    );
}
