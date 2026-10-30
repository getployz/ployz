use std::{
    fs,
    net::{Ipv4Addr, SocketAddr, SocketAddrV4},
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use hickory_server::proto::{
    op::{Message, Metadata, Query as WireQuery},
    rr::{Name, RData, Record, RecordType, rdata::A},
};
use ployz_core::{CopyRotation, MachineId};
use tempfile::TempDir;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpStream, UdpSocket},
    sync::Notify,
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

use super::{Backoff, DnsExit, Fixture, InstalledExe, serve_with};
use crate::{
    corrosion::fake_cluster::{ContainerFeed, FakeCluster, unclaimed_loopback},
    dns::{
        listeners::{Keeper, Listeners, MemoryKeeper},
        spec::{CorrosionEndpoint, DnsSpec, SpecFile},
        tests::{replica_observations, write_script},
    },
};

const TICK: Duration = Duration::from_millis(50);
const REFRESH: Duration = Duration::from_millis(200);
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
    refresh: Duration,
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
            listen: unclaimed_loopback(),
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
            refresh: REFRESH,
        }
    }

    fn spawn(&self, shutdown: &CancellationToken) -> JoinHandle<std::io::Result<DnsExit>> {
        let keeper = self.keeper.clone();
        let spec = self.spec_file.clone();
        let resolv_conf = self.dir.path().join("resolv.conf");
        let exe = InstalledExe::at(self.exe.clone()).unwrap();
        let refresh = self.refresh;
        let shutdown = shutdown.clone();
        tokio::spawn(async move {
            serve_with(
                Fixture {
                    spec,
                    keeper: &keeper,
                    resolv_conf,
                    exe,
                    tick: TICK,
                    refresh,
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
        eventually(move || async move {
            let answers = self.answers().await;
            if answers
                .as_ref()
                .is_some_and(|answers| answers.len() == count)
            {
                return Ok(());
            }
            Err(format!(
                "expected {count} replicas, last answer {answers:?}, statuses {:?}",
                self.keeper.statuses()
            ))
        })
        .await;
    }

    async fn ticks(&self, count: u64) {
        let mut ticks = self.keeper.subscribe_ticks();
        let from = *ticks.borrow_and_update();
        tokio::time::timeout(SETTLE, ticks.wait_for(|ticks| *ticks >= from + count))
            .await
            .expect("the process kept ticking")
            .unwrap();
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
async fn a_copy_sent_out_leaves_the_answers() {
    let harness = Harness::new();
    harness.publish_replicas(3).await;
    let shutdown = CancellationToken::new();
    let process = harness.spawn(&shutdown);
    harness.wait_for_replicas(3).await;

    let store = harness.cluster.store();
    for mut replica in replica_observations(3).into_iter().skip(1) {
        replica
            .try_update(|parts| parts.rotation = Some(CopyRotation::Out))
            .unwrap();
        store.publish_container(&replica).await.unwrap();
    }
    harness.wait_for_replicas(1).await;
    assert_eq!(
        harness.answers().await,
        Some(vec![Ipv4Addr::new(10, 210, 1, 1)])
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
        harness.ticks(1).await;
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

    let (cluster, rotated) = (&harness.cluster, &rotated);
    eventually(move || async move {
        let tokens = cluster.subscription_tokens();
        if tokens.contains(rotated) {
            Ok(())
        } else {
            Err(format!("subscribed with {tokens:?}"))
        }
    })
    .await;
    harness.publish_replicas(3).await;
    harness.wait_for_replicas(3).await;

    shutdown.cancel();
    assert_eq!(process.await.unwrap().unwrap(), DnsExit::Stopped);
}

#[tokio::test]
async fn a_quiet_subscription_is_caught_by_the_refresh() {
    let harness = Harness::new();
    harness.publish_replicas(2).await;
    let shutdown = CancellationToken::new();
    let process = harness.spawn(&shutdown);
    harness.wait_for_replicas(2).await;

    harness
        .cluster
        .feed_container_changes(ContainerFeed::Silent);
    harness.publish_replicas(3).await;
    harness.wait_for_replicas(3).await;
    assert!(
        !harness
            .keeper
            .statuses()
            .contains(&"reconnecting to Corrosion".to_owned()),
        "a quiet table is re-read, not reconnected"
    );

    shutdown.cancel();
    assert_eq!(process.await.unwrap().unwrap(), DnsExit::Stopped);
}

#[tokio::test]
async fn a_stalled_resnapshot_reconnects_on_the_refresh() {
    let harness = Harness::new();
    harness.publish_replicas(2).await;
    let shutdown = CancellationToken::new();
    let process = harness.spawn(&shutdown);
    harness.wait_for_replicas(2).await;

    harness
        .cluster
        .feed_container_changes(ContainerFeed::StalledSnapshot);
    // One change, so the stream stalls inside the re-snapshot rather than breaking it.
    let third = replica_observations(3).pop().unwrap();
    harness
        .cluster
        .store()
        .publish_container(&third)
        .await
        .unwrap();
    wait_for_status(&harness.keeper, "serving after reconnecting to Corrosion").await;
    harness.wait_for_replicas(3).await;

    shutdown.cancel();
    assert_eq!(process.await.unwrap().unwrap(), DnsExit::Stopped);
}

#[tokio::test]
async fn a_failed_rebuild_is_retried_on_the_tick() {
    let mut harness = Harness::new();
    // Only the membership tick may retry here, not the slow re-read.
    harness.refresh = Duration::from_secs(3600);
    harness.publish_replicas(2).await;
    let shutdown = CancellationToken::new();
    let process = harness.spawn(&shutdown);
    harness.wait_for_replicas(2).await;

    harness.cluster.fail_snapshots(true);
    harness.publish_replicas(3).await;
    harness.ticks(4).await;
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
    let (upstream, asked, answer) = held_upstream().await;
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
    tokio::time::timeout(SETTLE, asked.notified())
        .await
        .expect("the forwarded query never reached the upstream");
    shutdown.cancel();
    answer.notify_one();
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

    harness.spec.listen = unclaimed_loopback();
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
    wait_for_free_port(harness.spec.listen).await;

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
async fn an_unreadable_spec_idles_until_a_readable_one_arrives() {
    let harness = Harness::new();
    fs::write(harness.spec_file.path(), "not json").unwrap();
    let process = harness.spawn(&CancellationToken::new());
    wait_for_status(&harness.keeper, "idle").await;
    harness.ticks(5).await;
    assert!(
        !process.is_finished(),
        "an unreadable spec ended the process"
    );

    harness.spec_file.publish(Some(&harness.spec)).unwrap();
    assert_eq!(
        tokio::time::timeout(SETTLE, process)
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
    harness.ticks(4).await;
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
    wait_for_free_port(harness.spec.listen).await;
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
    harness.ticks(4).await;
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
    let mut statuses = keeper.subscribe_statuses();
    let seen = tokio::time::timeout(
        SETTLE,
        statuses.wait_for(|statuses| statuses.iter().any(|seen| seen == status)),
    )
    .await;
    assert!(seen.is_ok(), "{:?}", keeper.statuses());
}

/// A child another test forks keeps a copy of every socket this process had
/// open until it execs or exits, so a released port frees a moment later.
async fn wait_for_free_port(listen: SocketAddrV4) {
    eventually(move || async move {
        Listeners::bind(listen)
            .map(drop)
            .map_err(|error| format!("{listen} stayed busy: {error}"))
    })
    .await;
}

async fn eventually<F, Fut>(mut check: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<(), String>>,
{
    let deadline = Instant::now() + SETTLE;
    loop {
        let Err(last) = check().await else {
            return;
        };
        assert!(Instant::now() < deadline, "{last}");
        // real-wait: polls sockets and the fake Corrosion, which emit no event here
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn install(path: &std::path::Path, body: &str) {
    let staged = path.with_extension("new");
    write_script(&staged, body);
    fs::rename(staged, path).unwrap();
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

/// An upstream that signals `asked` on each query and answers it only once
/// `answer` is notified.
async fn held_upstream() -> (SocketAddr, Arc<Notify>, Arc<Notify>) {
    let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let address = socket.local_addr().unwrap();
    let asked = Arc::new(Notify::new());
    let answer = Arc::new(Notify::new());
    let (signal, release) = (asked.clone(), answer.clone());
    tokio::spawn(async move {
        let mut buf = [0u8; 4096];
        loop {
            let (len, peer) = socket.recv_from(&mut buf).await.unwrap();
            let mut message = Message::from_vec(buf.get(..len).unwrap()).unwrap();
            signal.notify_one();
            release.notified().await;
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
    (address, asked, answer)
}
