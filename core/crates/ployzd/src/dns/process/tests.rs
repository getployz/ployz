use std::{
    fs,
    net::{Ipv4Addr, SocketAddr, SocketAddrV4},
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    time::{Duration, Instant},
};

use hickory_server::proto::{
    op::{Message, Metadata, Query as WireQuery},
    rr::{Name, RData, Record, RecordType, rdata::A},
};
use ployz_core::MachineId;
use tempfile::TempDir;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpStream, UdpSocket},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

use super::{Backoff, DnsExit, Fixture, InstalledExe, serve_with};
use crate::{
    corrosion::fake_cluster::FakeCluster,
    dns::{
        listeners::{Keeper, Listeners, MemoryKeeper},
        spec::{CorrosionEndpoint, DnsSpec, SpecFile},
        tests::replica_observations,
    },
};

const TICK: Duration = Duration::from_millis(50);
const SETTLE: Duration = Duration::from_secs(5);
const SILENCE: Duration = Duration::from_millis(300);
const INTERNAL: &str = "api.app.internal.";
const FORWARDED: &str = "example.com.";

struct Harness {
    dir: TempDir,
    cluster: FakeCluster,
    keeper: MemoryKeeper,
    spec_file: SpecFile,
    spec: DnsSpec,
    exe: PathBuf,
}

impl Harness {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let cluster = FakeCluster::start();
        let spec_file = SpecFile::in_run_dir(dir.path());
        let token_file = dir.path().join("token");
        fs::write(&token_file, cluster.token()).unwrap();
        let spec = DnsSpec {
            machine: MachineId::random(),
            listen: free_loopback_port(),
            local_subnet: "127.0.0.0/8".parse().unwrap(),
            upstreams: Vec::new(),
            corrosion: CorrosionEndpoint {
                api: cluster.address(),
                admin_socket: dir.path().join("admin.sock"),
                token_file,
            },
        };
        spec_file.publish(Some(&spec)).unwrap();
        let exe = dir.path().join("ployzd");
        install(&exe, "exit 0");
        Self {
            dir,
            cluster,
            keeper: MemoryKeeper::default(),
            spec_file,
            spec,
            exe,
        }
    }

    fn spawn(&self, shutdown: &CancellationToken) -> JoinHandle<std::io::Result<DnsExit>> {
        let keeper = self.keeper.clone();
        let spec = self.spec_file.clone();
        let resolv_conf = self.dir.path().join("resolv.conf");
        let exe = InstalledExe::at(self.exe.clone()).unwrap();
        let shutdown = shutdown.clone();
        tokio::spawn(async move {
            serve_with(
                Fixture {
                    spec,
                    keeper: &keeper,
                    resolv_conf,
                    exe,
                    tick: TICK,
                    backoff: Backoff::between(
                        Duration::from_millis(20),
                        Duration::from_millis(100),
                    ),
                },
                shutdown,
            )
            .await
        })
    }

    async fn publish_replicas(&self, count: u16) {
        let store = self.cluster.store();
        for observation in replica_observations(count) {
            store.publish_container(&observation).await.unwrap();
        }
    }

    fn listen(&self) -> SocketAddr {
        self.spec.listen.into()
    }

    async fn answers(&self) -> Option<Vec<Ipv4Addr>> {
        ask_udp(self.listen(), INTERNAL, SILENCE)
            .await
            .map(|message| a_records(&message))
    }

    async fn wait_for_replicas(&self, count: usize) {
        let deadline = Instant::now() + SETTLE;
        loop {
            let answers = self.answers().await;
            if answers
                .as_ref()
                .is_some_and(|answers| answers.len() == count)
            {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "expected {count} replicas, last answer {answers:?}, statuses {:?}",
                self.keeper.statuses()
            );
            tokio::time::sleep(TICK).await;
        }
    }
}

#[tokio::test]
async fn stays_silent_until_the_first_projection_load() {
    let mut harness = Harness::new();
    harness.publish_replicas(2).await;
    harness.cluster.stop();
    let shutdown = CancellationToken::new();
    let process = harness.spawn(&shutdown);

    let early = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    wait_for_status(&harness.keeper, "loading").await;
    early
        .send_to(&query(INTERNAL), harness.listen())
        .await
        .unwrap();
    assert!(
        recv(&early, SILENCE).await.is_none(),
        "answered before any projection was loaded"
    );

    harness.cluster.restart();
    let queued = recv(&early, SETTLE)
        .await
        .expect("the queued query is answered after load");
    assert_eq!(a_records(&queued).len(), 2);
    assert!(
        harness
            .keeper
            .statuses()
            .contains(&"loading: Corrosion unavailable".to_owned()),
        "{:?}",
        harness.keeper.statuses()
    );

    shutdown.cancel();
    assert_eq!(process.await.unwrap().unwrap(), DnsExit::Stopped);
}

#[tokio::test]
async fn keeps_the_last_projection_while_corrosion_is_down() {
    let mut harness = Harness::new();
    harness.publish_replicas(2).await;
    let shutdown = CancellationToken::new();
    let process = harness.spawn(&shutdown);
    harness.wait_for_replicas(2).await;

    harness.cluster.stop();
    wait_for_status(&harness.keeper, "reconnecting to Corrosion").await;
    for _ in 0..5 {
        assert_eq!(harness.answers().await.map(|a| a.len()), Some(2));
        tokio::time::sleep(TICK).await;
    }

    shutdown.cancel();
    assert_eq!(process.await.unwrap().unwrap(), DnsExit::Stopped);
}

#[tokio::test]
async fn reloads_after_corrosion_returns() {
    let mut harness = Harness::new();
    harness.publish_replicas(2).await;
    let shutdown = CancellationToken::new();
    let process = harness.spawn(&shutdown);
    harness.wait_for_replicas(2).await;

    harness.cluster.stop();
    wait_for_status(&harness.keeper, "reconnecting to Corrosion").await;
    harness.cluster.restart();
    harness.publish_replicas(3).await;
    harness.wait_for_replicas(3).await;
    assert!(
        harness
            .keeper
            .statuses()
            .contains(&"serving after reconnecting to Corrosion".to_owned())
    );

    shutdown.cancel();
    assert_eq!(process.await.unwrap().unwrap(), DnsExit::Stopped);
}

#[tokio::test]
async fn a_rotated_token_reconnects_on_the_tick() {
    let harness = Harness::new();
    harness.publish_replicas(2).await;
    let shutdown = CancellationToken::new();
    let process = harness.spawn(&shutdown);
    harness.wait_for_replicas(2).await;
    let rotated = "b".repeat(64);
    fs::write(&harness.spec.corrosion.token_file, &rotated).unwrap();

    let deadline = Instant::now() + SETTLE;
    while !harness.cluster.subscription_tokens().contains(&rotated) {
        assert!(
            Instant::now() < deadline,
            "{:?}",
            harness.cluster.subscription_tokens()
        );
        tokio::time::sleep(TICK).await;
    }
    harness.publish_replicas(3).await;
    harness.wait_for_replicas(3).await;

    shutdown.cancel();
    assert_eq!(process.await.unwrap().unwrap(), DnsExit::Stopped);
}

#[tokio::test]
async fn a_failed_rebuild_is_retried_on_the_tick() {
    let harness = Harness::new();
    harness.publish_replicas(2).await;
    let shutdown = CancellationToken::new();
    let process = harness.spawn(&shutdown);
    harness.wait_for_replicas(2).await;

    harness.cluster.fail_snapshots(true);
    harness.publish_replicas(3).await;
    tokio::time::sleep(TICK * 4).await;
    assert_eq!(harness.answers().await.map(|a| a.len()), Some(2));

    harness.cluster.fail_snapshots(false);
    harness.wait_for_replicas(3).await;

    shutdown.cancel();
    assert_eq!(process.await.unwrap().unwrap(), DnsExit::Stopped);
}

// The daemon runs multi-threaded; there a stop can race the queued UDP send.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn drains_an_in_flight_forwarded_query_on_sigterm() {
    let mut harness = Harness::new();
    let upstream = slow_upstream(SILENCE).await;
    harness.spec.upstreams = vec![upstream];
    harness.spec_file.publish(Some(&harness.spec)).unwrap();
    harness.publish_replicas(1).await;
    let shutdown = CancellationToken::new();
    let process = harness.spawn(&shutdown);
    harness.wait_for_replicas(1).await;

    let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    client
        .send_to(&query(FORWARDED), harness.listen())
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    shutdown.cancel();
    let answered = recv(&client, SETTLE)
        .await
        .expect("the in-flight query is answered");
    assert_eq!(a_records(&answered), [Ipv4Addr::new(192, 0, 2, 1)]);
    assert_eq!(process.await.unwrap().unwrap(), DnsExit::Stopped);
}

#[tokio::test]
async fn a_changed_spec_ends_the_process_and_the_next_one_rebinds() {
    let mut harness = Harness::new();
    harness.publish_replicas(2).await;
    let first = harness.spawn(&CancellationToken::new());
    harness.wait_for_replicas(2).await;
    let old_names = harness.keeper.stored_names();

    harness.spec.listen = free_loopback_port();
    harness.spec_file.publish(Some(&harness.spec)).unwrap();
    assert_eq!(first.await.unwrap().unwrap(), DnsExit::SpecChanged);
    assert_eq!(harness.keeper.stored_names(), old_names);

    let shutdown = CancellationToken::new();
    let second = harness.spawn(&shutdown);
    harness.wait_for_replicas(2).await;
    let listeners = Listeners::bind(harness.spec.listen);
    assert_eq!(listeners.unwrap_err().kind(), std::io::ErrorKind::AddrInUse);
    assert_ne!(harness.keeper.stored_names(), old_names);
    assert_eq!(harness.keeper.stored_names().len(), 2);

    shutdown.cancel();
    assert_eq!(second.await.unwrap().unwrap(), DnsExit::Stopped);
}

#[tokio::test]
async fn a_removed_spec_ends_the_process_and_an_idle_process_holds_no_sockets() {
    let harness = Harness::new();
    harness.publish_replicas(1).await;
    let first = harness.spawn(&CancellationToken::new());
    harness.wait_for_replicas(1).await;

    harness.spec_file.publish(None).unwrap();
    assert_eq!(first.await.unwrap().unwrap(), DnsExit::SpecChanged);
    assert_eq!(harness.keeper.stored_names().len(), 2);

    let second = harness.spawn(&CancellationToken::new());
    wait_for_status(&harness.keeper, "idle").await;
    assert!(harness.keeper.stored_names().is_empty());
    assert!(Listeners::bind(harness.spec.listen).is_ok());

    harness.spec_file.publish(Some(&harness.spec)).unwrap();
    assert_eq!(
        tokio::time::timeout(SETTLE, second)
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
        DnsExit::SpecChanged
    );
}

#[tokio::test]
async fn inherited_sockets_carry_queries_queued_across_a_restart() {
    let harness = Harness::new();
    harness.publish_replicas(2).await;
    let shutdown = CancellationToken::new();
    let first = harness.spawn(&shutdown);
    harness.wait_for_replicas(2).await;
    shutdown.cancel();
    assert_eq!(first.await.unwrap().unwrap(), DnsExit::Stopped);
    assert_eq!(harness.keeper.stored_names().len(), 2);

    let udp = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    udp.send_to(&query(INTERNAL), harness.listen())
        .await
        .unwrap();
    let mut tcp = TcpStream::connect(harness.listen()).await.unwrap();
    send_tcp(&mut tcp, &query(INTERNAL)).await;
    assert!(
        recv(&udp, SILENCE).await.is_none(),
        "answered with no process running"
    );

    let shutdown = CancellationToken::new();
    let second = harness.spawn(&shutdown);
    let over_udp = recv(&udp, SETTLE).await.expect("queued UDP query answered");
    assert_eq!(a_records(&over_udp).len(), 2);
    let over_tcp = recv_tcp(&mut tcp, SETTLE)
        .await
        .expect("queued TCP query answered");
    assert_eq!(a_records(&over_tcp).len(), 2);

    shutdown.cancel();
    assert_eq!(second.await.unwrap().unwrap(), DnsExit::Stopped);
}

#[tokio::test]
async fn a_half_stored_pair_is_released_and_rebound() {
    let harness = Harness::new();
    harness.publish_replicas(2).await;
    let shutdown = CancellationToken::new();
    let first = harness.spawn(&shutdown);
    harness.wait_for_replicas(2).await;
    shutdown.cancel();
    assert_eq!(first.await.unwrap().unwrap(), DnsExit::Stopped);
    let tcp: Vec<String> = harness
        .keeper
        .stored_names()
        .into_iter()
        .filter(|name| name.starts_with("tcp-"))
        .collect();
    harness.keeper.forget(&tcp).unwrap();

    let shutdown = CancellationToken::new();
    let second = harness.spawn(&shutdown);
    harness.wait_for_replicas(2).await;
    let mut stream = TcpStream::connect(harness.listen()).await.unwrap();
    send_tcp(&mut stream, &query(INTERNAL)).await;
    let over_tcp = recv_tcp(&mut stream, SETTLE)
        .await
        .expect("the rebound TCP listener answers");
    assert_eq!(a_records(&over_tcp).len(), 2);
    assert_eq!(harness.keeper.stored_names().len(), 2);

    shutdown.cancel();
    assert_eq!(second.await.unwrap().unwrap(), DnsExit::Stopped);
}

#[tokio::test]
async fn abdicates_only_when_the_installed_binary_cannot_serve_dns() {
    let harness = Harness::new();
    harness.publish_replicas(1).await;
    let shutdown = CancellationToken::new();
    let process = harness.spawn(&shutdown);
    harness.wait_for_replicas(1).await;

    install(
        &harness.exe,
        "[ \"$1\" = dns ] && [ \"$2\" = --probe ] && exit 0; exit 1",
    );
    tokio::time::sleep(TICK * 4).await;
    assert!(
        !process.is_finished(),
        "a serving successor must not end the process"
    );
    assert_eq!(harness.keeper.stored_names().len(), 2);

    install(&harness.exe, "exit 2");
    assert_eq!(
        tokio::time::timeout(SETTLE, process)
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
        DnsExit::Abdicated
    );
    assert!(harness.keeper.stored_names().is_empty());
    assert!(Listeners::bind(harness.spec.listen).is_ok());
}

#[tokio::test]
async fn an_inconclusive_probe_is_retried_on_the_next_tick() {
    let harness = Harness::new();
    harness.publish_replicas(1).await;
    let shutdown = CancellationToken::new();
    let process = harness.spawn(&shutdown);
    harness.wait_for_replicas(1).await;

    install(&harness.exe, "exit 2");
    fs::set_permissions(&harness.exe, fs::Permissions::from_mode(0o644)).unwrap();
    tokio::time::sleep(TICK * 4).await;
    assert!(
        !process.is_finished(),
        "a probe that could not run must not end the process"
    );

    fs::set_permissions(&harness.exe, fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(
        tokio::time::timeout(SETTLE, process)
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
        DnsExit::Abdicated
    );
}

async fn wait_for_status(keeper: &MemoryKeeper, status: &str) {
    let deadline = Instant::now() + SETTLE;
    while !keeper.statuses().iter().any(|seen| seen == status) {
        assert!(Instant::now() < deadline, "{:?}", keeper.statuses());
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn install(path: &std::path::Path, body: &str) {
    let staged = path.with_extension("new");
    fs::write(&staged, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(&staged, fs::Permissions::from_mode(0o755)).unwrap();
    fs::rename(staged, path).unwrap();
}

fn free_loopback_port() -> SocketAddrV4 {
    Listeners::bind_ephemeral().listen()
}

fn query(name: &str) -> Vec<u8> {
    let mut message = Message::query();
    message.add_query(WireQuery::query(
        Name::from_ascii(name).unwrap(),
        RecordType::A,
    ));
    message.to_vec().unwrap()
}

fn a_records(message: &Message) -> Vec<Ipv4Addr> {
    message
        .answers
        .iter()
        .filter_map(|record| {
            if let RData::A(A(address)) = &record.data {
                Some(*address)
            } else {
                None
            }
        })
        .collect()
}

async fn ask_udp(server: SocketAddr, name: &str, within: Duration) -> Option<Message> {
    let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    client.send_to(&query(name), server).await.unwrap();
    recv(&client, within).await
}

async fn recv(socket: &UdpSocket, within: Duration) -> Option<Message> {
    let mut buf = [0u8; 4096];
    let (len, _) = tokio::time::timeout(within, socket.recv_from(&mut buf))
        .await
        .ok()?
        .unwrap();
    Some(Message::from_vec(buf.get(..len).unwrap()).unwrap())
}

async fn send_tcp(stream: &mut TcpStream, message: &[u8]) {
    stream
        .write_u16(u16::try_from(message.len()).unwrap())
        .await
        .unwrap();
    stream.write_all(message).await.unwrap();
}

async fn recv_tcp(stream: &mut TcpStream, within: Duration) -> Option<Message> {
    let length = tokio::time::timeout(within, stream.read_u16())
        .await
        .ok()?
        .unwrap();
    let mut response = vec![0; usize::from(length)];
    stream.read_exact(&mut response).await.unwrap();
    Some(Message::from_vec(&response).unwrap())
}

async fn slow_upstream(delay: Duration) -> SocketAddr {
    let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let address = socket.local_addr().unwrap();
    tokio::spawn(async move {
        let mut buf = [0u8; 4096];
        loop {
            let (len, peer) = socket.recv_from(&mut buf).await.unwrap();
            let mut message = Message::from_vec(buf.get(..len).unwrap()).unwrap();
            tokio::time::sleep(delay).await;
            message.metadata = Metadata::response_from_request(&message.metadata);
            let name = message.queries.first().unwrap().name().clone();
            message.add_answer(Record::from_rdata(
                name,
                60,
                RData::A(A(Ipv4Addr::new(192, 0, 2, 1))),
            ));
            socket
                .send_to(&message.to_vec().unwrap(), peer)
                .await
                .unwrap();
        }
    });
    address
}
