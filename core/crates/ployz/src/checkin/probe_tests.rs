use std::net::{TcpListener as StdTcpListener, UdpSocket as StdUdpSocket};

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

fn free_port() -> u16 {
    let tcp = StdTcpListener::bind("127.0.0.1:0").unwrap();
    let port = tcp.local_addr().unwrap().port();
    StdUdpSocket::bind(("127.0.0.1", port)).map_or_else(|_| free_port(), |_| port)
}

fn handout(handout: &str, window_ms: u64, listen: &[Port], send: &[u16]) -> Handout {
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

fn bindable(port: u16) -> bool {
    StdTcpListener::bind(("0.0.0.0", port)).is_ok() && StdUdpSocket::bind(("0.0.0.0", port)).is_ok()
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
        .open(handout("h4ndout", 5_000, &[Port::Tcp(port)], &[]), &key())
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
        .open(handout("long", 60_000, &[Port::Udp(port)], &[]), &key())
        .await;

    probes.window.take().unwrap().await.unwrap();

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
        )
        .await;
    greeting_from(tcp).await;

    eventually(|| bindable(tcp) && bindable(udp)).await;
    assert_eq!(probes.report().unwrap().handout, "short");
}

#[tokio::test]
async fn close_frees_every_port_before_it_returns() {
    let (tcp, udp) = (free_port(), free_port());
    let mut probes = Probes::default();
    probes
        .open(
            handout("closing", 10_000, &[Port::Tcp(tcp), Port::Udp(udp)], &[]),
            &key(),
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
                5_000,
                &[Port::Tcp(held_tcp), Port::Udp(held_udp), Port::Tcp(free)],
                &[],
            ),
            &key(),
        )
        .await;

    greeting_from(free).await;
    assert_eq!(
        probes.report().unwrap().bind_failed,
        [format!("tcp:{held_tcp}"), format!("udp:{held_udp}")]
    );
}

#[tokio::test]
async fn peers_hear_each_other_once_by_address() {
    let (a, b) = (free_port(), free_port());
    let (mut first, mut second) = (Probes::default(), Probes::default());
    first
        .open(handout("a", 5_000, &[Port::Udp(a)], &[b]), &key())
        .await;
    second
        .open(handout("b", 5_000, &[Port::Udp(b)], &[a]), &key())
        .await;

    eventually(|| {
        [&first, &second]
            .iter()
            .all(|probes| !probes.report().unwrap().heard_from.is_empty())
    })
    .await;
    tokio::time::sleep(RESEND * 2).await;
    for probes in [&first, &second] {
        assert_eq!(probes.report().unwrap().heard_from, ["127.0.0.1"]);
    }
}

#[tokio::test]
async fn only_the_exact_nonce_counts_as_heard() {
    let port = free_port();
    let mut probes = Probes::default();
    probes
        .open(handout("nonce", 5_000, &[Port::Udp(port)], &[]), &key())
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
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(probes.report().unwrap().heard_from.is_empty());

    let right = UdpSocket::bind("[::1]:0").await.unwrap();
    right
        .send_to(NONCE.as_bytes(), ("::1", port))
        .await
        .unwrap();
    eventually(|| probes.report().unwrap().heard_from == ["::1"]).await;
}

#[tokio::test]
async fn a_new_handout_replaces_the_window_even_with_the_same_nonce() {
    let port = free_port();
    let mut probes = Probes::default();
    probes
        .open(handout("first", 10_000, &[Port::Tcp(port)], &[]), &key())
        .await;
    greeting_from(port).await;

    probes
        .open(handout("second", 10_000, &[Port::Tcp(port)], &[]), &key())
        .await;

    assert_eq!(
        greeting_from(port).await,
        greeting("second", key().public_hex(), port).into_bytes()
    );
    let report = probes.report().unwrap();
    assert_eq!(report.handout, "second");
    assert!(report.bind_failed.is_empty());
}

#[test]
fn a_wait_carries_the_handout_and_an_unreadable_one_leaves_the_wait() {
    let body = br#"{"kind":"wait","retryAfter":10,"probe":{"nonce":"n0nce-n0nce-n0nce-n0nc","handout":"h","windowMs":10000,"listen":["udp:51820","tcp:80","tcp:443"],"send":[{"target":"203.0.113.7","port":51820}]}}"#;
    let Answer::Probe(after, handout) = wire::answer(body) else {
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
        Answer::Wait(Duration::from_secs(10))
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
